#include "profiler_state.hpp"

namespace Datadog {

ProfilerState&
ProfilerState::get()
{
    static ProfilerState instance;
    return instance;
}

void
ProfilerState::postfork_child()
{
    native_call_registry.postfork_child();
}

} // namespace Datadog
