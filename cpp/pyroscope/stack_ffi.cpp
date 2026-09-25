#include "dd_wrapper/include/profiler_state.hpp"
#include "sampler.hpp"
#include "thread_span_links.hpp"

#include "echion/echion_sampler.h"
#include "echion/vm.h"

#include <cstddef>
#include <cstdint>
#include <mutex>

/* Pyroscope patch: stands in for stack.py::_init's setter block. Adaptive
 * sampling and fast copy are off as first-iteration choices, not defaults --
 * see stack_todo.md. */
extern "C" void
pyroscope_stack_configure(double interval_s)
{
    set_fast_copy_enabled(false);
    Datadog::Sampler::get().set_adaptive_sampling(false);
    Datadog::Sampler::get().set_interval(interval_s);
}

extern "C" bool
pyroscope_stack_is_safe_copy_failed()
{
#if defined PL_LINUX
    return failed_safe_copy;
#else
    return false;
#endif
}

extern "C" bool
pyroscope_stack_start()
{
    return Datadog::Sampler::get().start();
}

extern "C" void
pyroscope_stack_stop()
{
    Datadog::Sampler::get().stop();
}

extern "C" void
pyroscope_stack_bump_upload_seq()
{
    Datadog::ProfilerState::get().upload_seq.fetch_add(1, std::memory_order_relaxed);
}

extern "C" uint64_t
pyroscope_stack_upload_seq()
{
    return Datadog::ProfilerState::get().upload_seq.load(std::memory_order_relaxed);
}

extern "C" void
pyroscope_stack_register_thread(uint64_t id, uint64_t native_id, const char* name)
{
    Datadog::Sampler::get().register_thread(id, native_id, name);
}

extern "C" void
pyroscope_stack_unregister_thread(uint64_t id)
{
    Datadog::Sampler::get().unregister_thread(id);
    Datadog::ThreadSpanLinks::get_instance().unlink_span(id);
}

extern "C" size_t
pyroscope_stack_thread_count()
{
    auto& echion = Datadog::Sampler::get().get_echion();
    const std::lock_guard<std::mutex> guard{ echion.thread_info_map_lock() };
    return echion.thread_info_map().size();
}
