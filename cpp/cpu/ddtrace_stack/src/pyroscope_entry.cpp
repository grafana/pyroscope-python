// Pyroscope C ABI for the vendored dd-trace-py stack (echion) sampler.
//
// Replaces upstream's src/stack.cpp, which was a CPython extension module
// (PyMethodDef stack_start/stack_stop/register_thread/link_span/
// track_greenlet/...) driven from ddtrace/profiling/collector/stack.py. We
// vendor no Python, so this file drives the sampler directly instead.
//
// Dropped along with the Python layer, because all of them are Datadog product
// features with no Pyroscope equivalent and no bearing on CPU sampling cost:
// span linking, greenlet tracking, asyncio task attribution, and sys.monitoring
// native call tracking.

#include <Python.h>

#include <cstdint>
#include <mutex>

#include "echion/config.h"
#include "echion/vm.h"
#include "sampler.hpp"

namespace {
std::mutex g_mutex;
bool g_running = false;
std::once_flag g_safe_copy_once;
} // namespace

extern "C" {

// Starts the echion sampling thread.
// `sample_rate_hz` is samples per second; `max_nframes` caps stack depth.
// Returns 0 on success, non-zero on failure (with a Python exception set).
int
pyroscope_cpu_ddtrace_start(uint32_t sample_rate_hz, uint32_t max_nframes)
{
    std::lock_guard<std::mutex> lock(g_mutex);
    if (g_running) {
        return 1;
    }
    if (sample_rate_hz == 0) {
        return 2;
    }

    // Install the SIGSEGV/SIGBUS recovery handlers and pick a memory-copy
    // strategy. Upstream does this from a library constructor; we patched that
    // out so importing pyroscope does not install signal handlers for users who
    // never select this profiler. See the Pyroscope patch in src/echion/vm.cc.
    //
    // Deliberately never torn down on stop: the handler chains to the previous
    // disposition for anything it does not own, so leaving it installed is
    // benign, whereas uninstalling it while fast_copy_active stays true would
    // leave safe_memcpy without its recovery path on a subsequent restart
    // (Sampler::sampling_thread only reinstalls under its own std::once_flag).
    std::call_once(g_safe_copy_once, []() { pyroscope_init_safe_copy(); });

    auto& sampler = Datadog::Sampler::get();

    // Sampler::set_interval takes FRACTIONAL SECONDS, not microseconds -- see
    // upstream stack.cpp's stack_set_interval ("Assumes the interval is given
    // in fractional seconds"). Passing microseconds here makes the sampling
    // thread sleep for hours and the stop path time out waiting for it.
    const double interval_s = 1.0 / static_cast<double>(sample_rate_hz);
    sampler.set_interval(interval_s);

    // echion reads the depth cap from this global rather than from Sampler.
    if (max_nframes > 0) {
        max_frames = max_nframes;
    }

    // Adaptive sampling varies the interval to hit a CPU-overhead target. That
    // is a sensible default for Datadog, but it makes the sample rate the user
    // asked for a suggestion rather than a setting, and it quietly trades
    // samples for overhead under load. Pin the interval instead.
    sampler.set_adaptive_sampling(false);

    if (!sampler.start()) {
        PyErr_SetString(PyExc_RuntimeError, "ddtrace stack profiler: failed to start sampling thread");
        return 3;
    }

    g_running = true;
    return 0;
}

void
pyroscope_cpu_ddtrace_stop(void)
{
    std::lock_guard<std::mutex> lock(g_mutex);
    if (!g_running) {
        return;
    }
    Datadog::Sampler::get().stop();
    g_running = false;
}

// Disarm in a freshly forked child.
//
// Deliberately does NOT touch the Sampler: the pthread_atfork child handler
// registered by Sampler::one_time_setup() has already run
// stack_postfork_cleanup(), which resets echion's mutexes and maps. Calling
// Sampler::postfork_child() a second time here would redo that work on state
// the first pass already rebuilt.
//
// g_mutex is not taken: it may have been held by a thread that no longer
// exists, and only this (single) thread runs in the child.
void
pyroscope_cpu_ddtrace_postfork_child(void)
{
    g_running = false;
}

} // extern "C"
