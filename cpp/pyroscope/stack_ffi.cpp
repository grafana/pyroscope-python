#include "dd_wrapper/include/profiler_state.hpp"
#include "sampler.hpp"
#include "thread_span_links.hpp"

#include "echion/echion_sampler.h"

#include <cstddef>
#include <cstdint>
#include <mutex>

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
