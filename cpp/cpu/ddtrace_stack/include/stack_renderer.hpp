#pragma once

// Pyroscope replacement for dd-trace-py's include/stack_renderer.hpp.
//
// Upstream's StackRenderer renders into Datadog::Sample -> ddup_* ->
// libdd_wrapper.so -> Rust libdatadog, which owns pprof encoding and upload.
// None of that chain is vendored here: Pyroscope has its own encoder and
// uploader, and shipping a second one would mean two pprof encoders and two
// uploaders in the same process.
//
// This class keeps the exact method surface echion calls (EchionSampler holds
// a StackRenderer by value and src/echion/*.cc call it directly), but renders
// into Pyroscope::CpuSample, which pushes to the Rust sink over the C ABI in
// rust/include/pyroscope_ffi.h. It is the same seam the memalloc port used
// when it replaced Datadog's ddup_interface with cpp/Pyroscope.h.

#include <cstdint>
#include <string>
#include <string_view>

#include "python_headers.hpp"

#include "echion/frame.h"
#include "echion/timing.h"

#include "PyroscopeCpu.h"

namespace Datadog {

enum class MetricType : std::uint8_t
{
    Time,
    Memory
};

struct ThreadState
{
    // Current thread info. One instance per StackRenderer is enough because the
    // renderer visits threads one at a time.
    uintptr_t id = 0;
    unsigned long native_id = 0;
    std::string name;
    microsecond_t wall_time_ns = 0;
    microsecond_t cpu_time_ns = 0;
    int64_t now_time_ns = 0;
};

class StackRenderer
{
    // The sample currently being built, or "no sample in flight" when
    // sample_active is false. Reused across samples so the sampling thread does
    // not allocate per stack.
    Pyroscope::CpuSample sample;
    bool sample_active = false;
    ThreadState thread_state = {};
    uint32_t pid = 0;

  public:
    StackRenderer();

    void render_thread_begin(PyThreadState* tstate,
                             std::string_view name,
                             microsecond_t wall_time_us,
                             uintptr_t thread_id,
                             unsigned long native_id);
    void render_task_begin(std::string_view task_name, bool on_cpu, uint64_t task_id);
    void render_frame(Frame& frame);
    void render_cpu_time(microsecond_t cpu_time_us);
    void render_native_frame(const std::string& name, const std::string& module);
    void render_stack_end();

    // Drop a partially-built sample without flushing it.
    void abort_sample();

    // Reset in-flight state after a fork.
    void postfork_child();
};

} // namespace Datadog
