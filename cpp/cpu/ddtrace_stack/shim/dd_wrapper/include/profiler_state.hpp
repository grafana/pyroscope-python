#pragma once

// Pyroscope shim for dd_wrapper/include/profiler_state.hpp.
//
// Upstream's ProfilerState is almost entirely libdatadog state (exporter
// config, ddog_prof_* interned string caches, upload cancellation tokens).
// None of that is vendored. The only member the code we keep actually touches
// is native_call_registry, used by src/echion/stacks.cc to inject native frames
// discovered via sys.monitoring CALL events.
//
// That registry is populated exclusively from the Python layer
// (start_native_monitoring in upstream's src/stack.cpp), which we do not
// vendor, so it is always empty and lookup() always misses. Keeping the shim
// rather than editing stacks.cc lets that file stay byte-identical to upstream.

#include <cstdint>
#include <functional>
#include <optional>
#include <string>

#include "constants.hpp"

namespace Datadog {

struct NativeCallEntry
{
    std::string name;
    std::string module;
};

class NativeCallRegistry
{
  public:
    std::optional<std::reference_wrapper<NativeCallEntry>> lookup(uintptr_t /*code_ptr*/,
                                                                  int /*offset_bytes*/,
                                                                  int /*first_lineno*/)
    {
        // Always a miss: native call tracking needs the Python layer we drop.
        return std::nullopt;
    }
};

class ProfilerState
{
  public:
    static ProfilerState& get()
    {
        static ProfilerState instance;
        return instance;
    }

    NativeCallRegistry native_call_registry{};

    // Sample configuration echion consults for stack depth limits.
    unsigned int max_nframes{ g_default_max_nframes };

  private:
    ProfilerState() = default;
};

} // namespace Datadog
