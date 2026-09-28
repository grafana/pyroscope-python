#include "dd_wrapper/include/profiler_state.hpp"
#include "sampler.hpp"
#include "thread_span_links.hpp"

#include "echion/echion_sampler.h"
#include "echion/vm.h"

#include <cstddef>
#include <cstdint>
#include <mutex>

/* Pyroscope patch: stands in for stack.py::_init's setter block. Handlers are
 * installed at most once per process: reinstalling over a foreign handler would
 * undo sampling_thread's permanent fallback. Adaptive sampling stays off. */
extern "C" void
pyroscope_stack_configure(double interval_s, bool fast_copy, double fast_copy_warmup_s)
{
    static std::once_flag safe_copy_once;
    std::call_once(safe_copy_once, init_safe_copy, fast_copy);
    set_fast_copy_enabled(safe_memcpy_initialized);
    Datadog::Sampler::get().set_adaptive_sampling(false);
    Datadog::Sampler::get().set_interval(interval_s);
    Datadog::Sampler::get().set_fast_copy_warmup_seconds(fast_copy_warmup_s);
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

extern "C" uint8_t
pyroscope_stack_pause_sampling()
{
    return static_cast<uint8_t>(Datadog::Sampler::get().pause());
}

extern "C" void
pyroscope_stack_resume_sampling()
{
    Datadog::Sampler::get().resume();
}

extern "C" void
pyroscope_stack_uninstall_segv_handler()
{
    if (fast_copy_active) {
        uninstall_segv_handler();
    }
}

extern "C" void
pyroscope_stack_reinstall_segv_handler()
{
    if (fast_copy_active) {
        init_segv_catcher();
    }
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
