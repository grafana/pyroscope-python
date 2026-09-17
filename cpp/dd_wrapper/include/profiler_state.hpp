#pragma once

/* Pyroscope patch: a drastically trimmed stand-in for dd_wrapper's
 * Datadog::ProfilerState.
 *
 * Upstream this is the profiler's one global-state object, and almost all of
 * it is libdatadog: the Profiles Dictionary handle plus its init/release
 * lifecycle, the ddog_prof_StringId2 tag/label key caches and the interned
 * empty-string id, the Profile (ddog_prof_Profile + ProfilerStats) it owns,
 * the whole uploader configuration block (env/service/version/url/tags/...),
 * and the upload_lock / ddog_CancellationToken upload_cancel pair. Pyroscope
 * has no libdatadog and no dd_wrapper uploader, so none of that is carried
 * over; the profile-building state it stands in for lives on the Rust side
 * (rust/src/encode/).
 *
 * What remains is exactly what the vendored stack sampler reaches for, and
 * both of those members are plain C++ upstream too:
 *
 *   * native_call_registry -- the sys.monitoring CALL-event side table that
 *     lets the sampler splice native frames in front of their Python caller.
 *     Copied verbatim; see native_call_tracker.hpp.
 *   * upload_seq -- a counter the sampler watches to notice upload boundaries.
 *
 * Also dropped: start(), cleanup(), prefork(), postfork_parent(),
 * is_initialized(). Upstream's start() is what creates the Profiles
 * Dictionary and installs dd_wrapper's pthread_atfork handlers; there is
 * nothing here to initialize, and Sampler installs its own handlers.
 *
 * TODO(Pyroscope): nothing increments upload_seq. Upstream bumps it once per
 * upload in the uploader, and Sampler::sampling_thread uses the delta to clear
 * echion's ephemeral string table entries (task and greenlet names) every 25
 * uploads -- see the ephemeral_clear_interval block in cpp/stack/src/sampler.cpp.
 * Held at 0, that clear never runs and the ephemeral table grows without bound
 * for a process that churns asyncio task names. Bump it from the Rust dump
 * path (memory::implementation::dump_pprof, or the CPU equivalent) when the CPU
 * profile is actually wired up to the encoder. */

#include "native_call_tracker.hpp"

#include <atomic>
#include <cstdint>

namespace Datadog {

class ProfilerState
{
  public:
    // Singleton access
    static ProfilerState& get();

    // ========================================================================
    // Native call tracking state
    // ========================================================================
    NativeCallRegistry native_call_registry{};

    // ========================================================================
    // Upload state
    // ========================================================================
    std::atomic<uint64_t> upload_seq{ 0 };

    /* TODO(Pyroscope): no caller yet, so the native call registry's mutex is
     * never re-initialized in a forked child.
     *
     * Upstream never calls this from the stack tree either -- it runs from the
     * pthread_atfork child handler that ProfilerState::start installs. That
     * handler is registered before Sampler::start, so POSIX's FIFO child-handler
     * ordering guarantees it runs before the sampler's own; the note in
     * Sampler::atfork_child in cpp/stack/src/sampler.cpp still describes that
     * arrangement. We have no ProfilerState::start, so the ordering does not
     * hold and nothing re-inits the mutex.
     *
     * Wiring this up needs the same care as the TODO(Pyroscope) on
     * ffikit::stop_profilers in rust/src/ffikit.rs, which flags the mirror
     * problem: stack_atfork_child calls restart_after_fork() before Python's
     * at_fork_after_in_child hooks run. */
    void postfork_child();

  private:
    ProfilerState() = default;
    ~ProfilerState() = default;

    // Non-copyable, non-movable
    ProfilerState(const ProfilerState&) = delete;
    ProfilerState& operator=(const ProfilerState&) = delete;
    ProfilerState(ProfilerState&&) = delete;
    ProfilerState& operator=(ProfilerState&&) = delete;
};

} // namespace Datadog
