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
 *     Stubbed to always-empty; see native_call_tracker.hpp.
 *   * upload_seq -- a counter the sampler watches to notice upload boundaries.
 *
 * Also dropped: start(), cleanup(), prefork(), postfork_parent(),
 * postfork_child(), is_initialized(). Upstream's start() is what creates the
 * Profiles Dictionary and installs dd_wrapper's pthread_atfork handlers; there
 * is nothing here to initialize, and Sampler installs its own handlers.
 */

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
