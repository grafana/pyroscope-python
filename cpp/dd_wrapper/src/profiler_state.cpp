#include "profiler_state.hpp"

namespace Datadog {

ProfilerState&
ProfilerState::get()
{
    static ProfilerState instance;
    return instance;
}

} // namespace Datadog
