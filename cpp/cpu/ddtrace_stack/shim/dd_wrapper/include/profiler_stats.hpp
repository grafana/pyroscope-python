#pragma once

// Pyroscope shim for dd_wrapper/include/profiler_stats.hpp.
//
// Upstream accumulates profiler telemetry into a ProfilerStats object that is
// uploaded to Datadog alongside the profile via libdatadog. Pyroscope has no
// equivalent channel and deliberately vendors no part of libdatadog, so every
// setter here is a no-op.
//
// Keeping the full method surface (rather than deleting the call sites) is what
// lets src/sampler.cpp stay byte-identical to upstream, which is what makes
// re-vendoring cheap. See ../../../VENDOR.md.

#include <cstddef>
#include <cstdint>

namespace Datadog {

class ProfilerStats
{
  public:
    // Templated so the shim does not have to track upstream's exact parameter
    // types; every one of these discards its argument.
    template<typename T> void set_fast_copy_memory_user_disabled(T) {}
    template<typename T> void set_fast_copy_memory_capable(T) {}
    template<typename T> void set_fast_copy_memory_syscall_fallback(T) {}
    template<typename T> void set_fast_copy_memory_enabled(T) {}
    template<typename T> void set_sampling_interval_us(T) {}
    template<typename T> void set_string_table_count(T) {}
    template<typename T> void set_asyncio_task_count(T) {}
    template<typename T> void set_greenlet_count(T) {}
    template<typename T> void add_copy_memory_error_count(T) {}
    template<typename T> void add_sample_capture_cpu_time_us(T) {}
    void increment_sample_count() {}
    void increment_sampling_event_count() {}
};

} // namespace Datadog
