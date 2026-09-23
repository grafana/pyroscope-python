// Pyroscope replacement for dd-trace-py's src/stack_renderer.cpp.
// See include/stack_renderer.hpp for why upstream's version is not vendored.
//
// echion pushes frames leaf-first as it unwinds, which is the order
// Pyroscope::CpuSample and py-spy both expect, so no reversal is needed.

#include "stack_renderer.hpp"

#include <unistd.h>

#include "echion/echion_sampler.h"
#include "echion/strings.h"
#include "sampler.hpp"

namespace Datadog {

namespace {
constexpr std::string_view kMissingFilename = "<unknown file>";
constexpr std::string_view kMissingName = "<unknown function>";

// Deepest stack we accumulate. echion enforces its own max_frames; this is a
// backstop so a runaway unwind cannot grow the sample without bound.
constexpr size_t kMaxFrames = 512;
} // namespace

StackRenderer::StackRenderer()
  : sample(kMaxFrames)
{
    pid = static_cast<uint32_t>(getpid());
}

void
StackRenderer::render_thread_begin(PyThreadState* /*tstate*/,
                                   std::string_view name,
                                   microsecond_t wall_time_us,
                                   uintptr_t thread_id,
                                   unsigned long native_id)
{
    // A previous stack that never saw render_stack_end (e.g. the unwind
    // failed) would otherwise bleed its frames into this one.
    sample.clear();
    sample_active = true;

    thread_state.id = thread_id;
    thread_state.native_id = native_id;
    thread_state.name = std::string(name);
    thread_state.wall_time_ns = 1000 * wall_time_us;
    thread_state.cpu_time_ns = 0; // filled in by render_cpu_time

    sample.set_pid(pid);
    sample.set_thread(static_cast<uint64_t>(thread_id), thread_state.name);
}

void
StackRenderer::render_task_begin(std::string_view /*task_name*/, bool /*on_cpu*/, uint64_t /*task_id*/)
{
    // Pyroscope does not model asyncio tasks as a separate dimension, so a
    // task simply continues the current thread's sample. If echion starts a
    // task without a preceding thread_begin, treat it as the start of a stack
    // so its frames are not dropped.
    if (!sample_active) {
        sample.clear();
        sample_active = true;
        sample.set_pid(pid);
        sample.set_thread(static_cast<uint64_t>(thread_state.id), thread_state.name);
    }
}

void
StackRenderer::render_frame(Frame& frame)
{
    if (!sample_active) {
        return;
    }

    // The frame is owned by an LRU cache that may have evicted it, leaving the
    // string table keys stale. That is per-frame, not fatal, so fall back to
    // placeholders rather than dropping the whole sample.
    const auto& string_table = Sampler::get().get_echion().string_table();

    std::string_view name_str = kMissingName;
    auto maybe_name = string_table.lookup(frame.name);
    if (maybe_name) {
        name_str = maybe_name->get();
    }

    std::string_view filename_str = kMissingFilename;
    auto maybe_filename = string_table.lookup(frame.filename);
    if (maybe_filename) {
        filename_str = maybe_filename->get();
    }

    // Safe to borrow: the string table owns these std::strings and does not
    // mutate them while we hold the sampling thread, and export_sample() runs
    // before we return to the sampler loop.
    sample.push_frame(name_str, filename_str, static_cast<int>(frame.line));
}

void
StackRenderer::render_cpu_time(microsecond_t cpu_time_us)
{
    // This is the CPU time this thread actually consumed since the previous
    // sampling tick, read from its per-thread CPU clock. It is what makes the
    // sample a CPU sample; see render_stack_end().
    thread_state.cpu_time_ns = 1000 * cpu_time_us;
}

void
StackRenderer::render_native_frame(const std::string& name, const std::string& module)
{
    if (!sample_active) {
        return;
    }
    // Native frames carry no line number.
    sample.push_frame(name, module, 0);
}

void
StackRenderer::render_stack_end()
{
    if (!sample_active) {
        return;
    }

    // Weight the sample by the CPU time this thread actually burned, NOT by
    // one sampling period.
    //
    // This sampler is wall-clock driven: it wakes on an interval and walks
    // every Python thread, so it produces one stack per thread per tick
    // whether or not that thread ran. Crediting each of those a full period of
    // CPU inflates the profile by roughly the thread count -- it will happily
    // report more CPU than the process could possibly have consumed -- and
    // attributes CPU to threads that were blocked on I/O or waiting for the
    // GIL.
    //
    // A thread that consumed no CPU since the last tick contributes nothing;
    // export_sample() drops a zero-CPU sample.
    sample.set_cpu_nanos(thread_state.cpu_time_ns > 0
                           ? static_cast<uint64_t>(thread_state.cpu_time_ns)
                           : 0);
    sample.export_sample();
    sample.clear();
    // Consume the delta so it cannot be counted twice if the sampler renders
    // several stacks (e.g. tasks) for the same thread in one tick.
    thread_state.cpu_time_ns = 0;
    sample_active = false;
}

void
StackRenderer::abort_sample()
{
    sample.clear();
    sample_active = false;
}

void
StackRenderer::postfork_child()
{
    // Any sample in flight belonged to a thread that did not survive the fork.
    abort_sample();
    pid = static_cast<uint32_t>(getpid());
}

} // namespace Datadog
