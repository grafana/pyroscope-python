#pragma once

#include <cstdint>
#include <functional>
#include <optional>
#include <string>

namespace Datadog {

struct NativeCallEntry
{
    std::string name;
    std::string module;
};

// Pyroscope patch: an always-empty stub. Nothing registers call sites (the
// sys.monitoring tracker lived in upstream stack.cpp), so this drops the map,
// its shared_mutex and native_call_tracker.cpp, keeping echion/stacks.cc verbatim.
class NativeCallRegistry
{
  public:
    std::optional<std::reference_wrapper<NativeCallEntry>> lookup(uintptr_t, int, int) { return std::nullopt; }
};

} // namespace Datadog
