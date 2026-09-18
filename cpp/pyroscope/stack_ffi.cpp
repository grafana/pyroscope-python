#include "dd_wrapper/include/profiler_state.hpp"

#include <cstdint>

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
