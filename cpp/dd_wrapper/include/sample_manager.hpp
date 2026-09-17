#pragma once

/* Pyroscope patch: a two-method stand-in for dd_wrapper's SampleManager.
 *
 * Only start_sample/drop_sample are kept. Upstream's configuration setters
 * (add_type, set_max_nframes, set_timeline, set_sample_pool_capacity) all
 * write to ProfilerState fields that our trimmed ProfilerState does not carry,
 * and nothing in the vendored stack tree calls them -- the Python-side ddup
 * config path that did is not vendored. */

#include "constants.hpp"
#include "sample.hpp"

namespace Datadog {

class SampleManager
{
  public:
    /* Hand out the sample the renderer is about to fill in.
     *
     * Upstream draws from StaticSamplePool -- a four-slot lock-free array of
     * std::atomic<Sample*> -- and falls back to `new Sample(type_mask,
     * max_nframes)`, which is why upstream's callers treat a null return as
     * "allocation failed, disable the sampler". Ours never returns null, but
     * do not simplify those call sites: keeping the check means a real pool
     * can be dropped back in without touching stack_renderer.cpp.
     *
     * A single thread_local instance replaces the pool. Two invariants make
     * that sound, both of which hold today:
     *
     *   * Only the sampling thread renders. start_sample is reached from
     *     StackRenderer::render_thread_begin and render_task_begin, both of
     *     which run on the thread Sampler::sampling_thread owns.
     *   * A sample is always finished before the next one starts.
     *     render_stack_end pairs flush_sample() with drop_sample() and nulls
     *     the renderer's pointer, so no two live samples ever overlap.
     *
     * Consequently there is nothing to allocate, nothing to free, and no way
     * for one thread's in-flight sample to be handed to another. It also side-
     * steps the leak upstream documents on its `new` path (a fork between
     * start_sample and drop_sample orphans the allocation).
     *
     * clear() here, not in drop_sample, mirrors StaticSamplePool::return_sample's
     * reason for clearing on the way in: stale per-sample state (a span id, say)
     * must not leak into the next user. */
    static Sample* start_sample()
    {
        static thread_local Sample sample{ g_default_max_nframes };
        sample.clear();
        return &sample;
    }

    /* No-op: start_sample's storage is thread_local and outlives the caller.
     * Kept so the renderer's start/drop pairing stays visible and correct for
     * whatever backs start_sample later. */
    static void drop_sample([[maybe_unused]] Sample* sample) {}
};

} // namespace Datadog
