#pragma once

// Pyroscope shim for dd_wrapper/include/sample.hpp.
//
// Upstream's Sample is the entry point into libdatadog's pprof builder and
// uploader. Pyroscope renders through its own StackRenderer into
// Pyroscope::CpuSample instead (see ../../../include/stack_renderer.hpp), so
// the only thing still needed from this header is the
// profile_borrow()/stats() handle that src/sampler.cpp uses for telemetry --
// all no-ops here.
//
// Deliberately does NOT include libdatadog_helpers.hpp (which is what declares
// the ddog_* types upstream); see ../../../VENDOR.md.

#include "profiler_stats.hpp"

namespace Datadog {

class Sample
{
  public:
    // Mirrors upstream's RAII borrow handle. The borrow is meaningless without
    // libdatadog, so this just hands back a process-wide no-op stats object.
    // Returned by value: src/sampler.cpp stores it as `auto borrow = ...`.
    class ProfileBorrow
    {
      public:
        ProfilerStats& stats() { return stats_; }

      private:
        // Shared by every borrow; nothing reads it back.
        inline static ProfilerStats stats_{};
    };

    static ProfileBorrow profile_borrow() { return {}; }
};

} // namespace Datadog
