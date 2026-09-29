#include "dd_wrapper/include/profiler_state.hpp"
#include "dd_wrapper/include/sample_manager.hpp"
#include "pyroscope_ffi.h"
#include "sampler.hpp"
#include "span_links.hpp"

#include "echion/echion_sampler.h"
#include "echion/vm.h"

#include <cmath>
#include <cstddef>
#include <cstdint>
#include <mutex>

static constexpr double g_min_target_overhead = 1e-4;

/* Pyroscope patch: stands in for stack.py::_init's setter block. Handlers are
 * installed at most once per process: reinstalling over a foreign handler would
 * undo sampling_thread's permanent fallback.
 *
 * target_overhead is a fraction here, matching adapt_sampling_interval's
 * formula and g_target_overhead. Upstream's stack.py passes a 1..100
 * percentage into the same setter.
 *
 * adapt_sampling_interval divides by target_overhead, then casts to int64_t
 * before clamping, so a small divisor makes that conversion undefined. The
 * floor keeps us clear of it by ~9 orders of magnitude; see
 * stack_known_bugs.md for why the cast itself is left alone. Every other
 * setter here either clamps or accepts its whole range. */
extern "C" void
pyroscope_stack_configure(double interval_s,
                          bool fast_copy,
                          double fast_copy_warmup_s,
                          uint32_t max_nframes,
                          uint32_t max_threads,
                          bool adaptive_sampling,
                          double target_overhead,
                          uint64_t max_sampling_period_us)
{
    static std::once_flag safe_copy_once;
    std::call_once(safe_copy_once, init_safe_copy, fast_copy);
    set_fast_copy_enabled(safe_memcpy_initialized);
    Datadog::SampleManager::set_max_nframes(max_nframes);
    auto& sampler = Datadog::Sampler::get();
    sampler.set_max_frames(max_nframes);
    sampler.set_max_threads_per_sample(max_threads);
    sampler.set_adaptive_sampling(adaptive_sampling);
    if (std::isfinite(target_overhead) && target_overhead >= g_min_target_overhead) {
        sampler.set_target_overhead(target_overhead);
    }
    sampler.set_max_sampling_period(static_cast<microsecond_t>(max_sampling_period_us));
    sampler.set_interval(interval_s);
    sampler.set_fast_copy_warmup_seconds(fast_copy_warmup_s);
}

extern "C" uint64_t
pyroscope_stack_interval_us()
{
    return Datadog::Sampler::get().get_interval_us();
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
pyroscope_stack_fast_copy_initialized()
{
    return safe_memcpy_initialized;
}

extern "C" bool
pyroscope_stack_start()
{
    return Datadog::Sampler::get().start();
}

/* Pyroscope patch: ffikit::stop_profilers clears the string table right after
 * this returns, so the renderer's ids have to be dropped here -- after stop()
 * has joined the sampling thread, and before the table goes away. */
extern "C" void
pyroscope_stack_stop()
{
    auto& sampler = Datadog::Sampler::get();
    sampler.stop();
    sampler.get_echion().renderer().reset_string_cache();
}

extern "C" SamplerPauseResult
pyroscope_stack_pause_sampling()
{
    return Datadog::Sampler::get().pause();
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
    Datadog::SpanLinks::get_instance().unlink_span(id);
}

extern "C" size_t
pyroscope_stack_thread_count()
{
    auto& echion = Datadog::Sampler::get().get_echion();
    const std::lock_guard<std::mutex> guard{ echion.thread_info_map_lock() };
    return echion.thread_info_map().size();
}
