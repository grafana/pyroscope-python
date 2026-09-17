//
// Created by korniltsev on 2/6/2026.
//
#pragma once

#include <string_view>
#include <vector>


extern "C" {
#include "pyroscope_ffi.h"
}

namespace Pyroscope
{
    /* No-op stand-in for Datadog's ProfilerStats.
     *
     * The vendored stack sampler reports per-cycle diagnostics (sample counts,
     * string table sizes, sampler self-time, ...) through this object. Datadog
     * ships them alongside the profile in an internal-metadata channel that
     * Pyroscope has no equivalent for, so every setter here deliberately drops
     * its argument. Names and arities mirror dd-trace-py's
     * dd_wrapper/include/profiler_stats.hpp so a real implementation can be
     * dropped in without touching any call site.
     *
     * Stateless by design: see ProfileBorrow for why that matters. */
    class ProfilerStats
    {
    public:
        void increment_sample_count([[maybe_unused]] size_t k_sample_count = 1)
        {
        }

        void increment_sampling_event_count([[maybe_unused]] size_t k_sampling_event_count = 1)
        {
        }

        void set_sampling_interval_us([[maybe_unused]] size_t interval_us)
        {
        }

        void set_string_table_count([[maybe_unused]] size_t count)
        {
        }

        void set_string_table_ephemeral_count([[maybe_unused]] size_t count)
        {
        }

        void set_fast_copy_memory_enabled([[maybe_unused]] bool enabled)
        {
        }

        void add_copy_memory_error_count([[maybe_unused]] size_t count)
        {
        }

        void add_sample_capture_cpu_time_us([[maybe_unused]] size_t cpu_time_us)
        {
        }

        void set_asyncio_task_count([[maybe_unused]] size_t count)
        {
        }

        void set_greenlet_count([[maybe_unused]] size_t count)
        {
        }

        void set_heap_tracker_size([[maybe_unused]] size_t count)
        {
        }
    };

    /* No-op stand-in for Datadog's ProfileBorrow.
     *
     * Upstream this is an RAII guard holding the profile mutex, which is what
     * lets the sampler batch several stats updates into one upload window
     * (`auto borrow = Sample::profile_borrow();`). ProfilerStats above carries
     * no state, so there is nothing to serialize and nothing to unlock; this is
     * an empty object and stats() hands back a shared instance.
     *
     * Non-copyable but movable, matching upstream, so the by-value `auto
     * borrow = ...` binding and the temporary-and-discard call style both
     * compile unchanged. */
    class ProfileBorrow
    {
    public:
        ProfileBorrow() = default;
        ProfileBorrow(const ProfileBorrow&) = delete;
        ProfileBorrow& operator=(const ProfileBorrow&) = delete;
        ProfileBorrow(ProfileBorrow&&) noexcept = default;
        ProfileBorrow& operator=(ProfileBorrow&&) noexcept = default;

        // Non-static to match upstream's call shape (`borrow.stats()`), even
        // though the instance carries nothing.
        ProfilerStats& stats() const
        {
            static ProfilerStats stats;
            return stats;
        }
    };

    /* Name the vendored profilers use for an interned string id. Upstream this
     * is a libdatadog handle typedef (ddog_prof_StringId2) from
     * dd_wrapper/include/sample.hpp, which Pyroscope does not vendor. Ours is
     * a plain u32 index into the process-wide table in
     * rust/src/encode/interner.rs. */
    using string_id = FFIInternedString;

    /* Stand-in for Datadog's intern_string.
     *
     * Interns into one process-wide table shared by every profiler in this
     * extension, so an id minted here is comparable across the memory
     * profiler and the vendored CPU stack sampler.
     *
     * Infallible, unlike Datadog::intern_string, which returns std::optional
     * because libdatadog's Profiles Dictionary can fail to allocate. Every
     * failure mode here -- null or empty input, poisoned table lock -- yields
     * index 0, the id of the empty string, which is itself a valid id. So
     * callers must NOT guard the result: there is no failure to handle, and
     * treating 0 as failure would wrongly discard genuinely empty strings
     * (an empty module name, say).
     *
     * The id stays valid until the table is cleared at agent teardown.
     * Anything caching ids across samples (see StackRenderer::string_id_cache)
     * must be discarded whenever the table is, or stale indices will silently
     * resolve to whatever string later occupies them. The invariant is spelled
     * out on interner::clear in rust/src/encode/interner.rs.
     *
     * `inline` is required: this header is included from several translation
     * units (memalloc's _memalloc_tb.h, the stack sampler's sampler.cpp and
     * stack_renderer.cpp), and without it each one emits the symbol. */
    inline string_id intern_string(const std::string_view s)
    {
        return pyroscope_string_table_intern_string(FFIStringView{
            .data = s.data(),
            .len = s.length()
        });
    }

    class Sample
    {
        std::vector<FFIFrame> frames;
        size_t max_nframes;
        FFIHeapSampleValues values{};

    public:
        explicit Sample(const size_t max_nframes) : max_nframes{max_nframes}
        {
            frames.reserve(max_nframes);
        }


        /* Push a frame whose strings are already interned.
         *
         * There is deliberately no Pyroscope::intern_function to go with
         * Pyroscope::intern_string. Upstream needs one because libdatadog's
         * Profiles Dictionary hands out opaque function handles that its
         * push_frame consumes; our FFIFrame carries the two string ids
         * directly, and the Rust encoder already interns functions on exactly
         * upstream's key -- see PProfBuilder::add_function_mirror, which
         * dedupes FunctionMirror{name, filename} (and, like upstream, leaves
         * system_name empty). It also dedupes locations, which upstream does
         * not intern either: libdatadog dedupes those per profile at add time.
         *
         * A process-wide function table would not work the way the string
         * table does: pprof function ids are per-profile and sequential, and
         * are written straight into profile.function[].id, so a process-wide
         * id is not a valid pprof id. Strings get away with it only because
         * the whole table is copied into every profile.
         *
         * This overload is for callers that keep their own id cache and hand
         * ids in (the vendored stack renderer); the string_view overload below
         * is for callers that do not (memalloc).
         *
         * TODO(Pyroscope): upstream's push_frame also takes a frame address,
         * which lands in ddog_prof_Location.address. We drop it: FFIFrame has
         * no field for it and add_location_mirror hardcodes address and
         * mapping_id to 0. Nothing of value is lost today -- the only caller
         * that passed a nonzero value used a literal 1 as an undocumented
         * sentinel for native frames, which stay distinguishable by their name
         * and filename anyway. */
        void push_frame(const string_id function_name, const string_id file_name, const int line)
        {
            if (frames.size() == max_nframes)
            {
                incr_dropped_frames();
                return;
            }
            frames.emplace_back(
                FFIFrame{
                    .function_name = function_name,
                    .file_name = file_name,
                    .line = line,
                }
            );
        }


        /* Interning overload, mirroring upstream's
         * push_frame(name, filename, address, line). The third parameter is
         * upstream's frame address and is ignored; see the TODO above.
         *
         * The capacity check is repeated here rather than left to the overload
         * above, because interning is not free and is not local: a frame we are
         * about to drop must not add its strings to the process-wide table. */
        void push_frame(const std::string_view function_name, const std::string_view file_name,
                        [[maybe_unused]] int address, const int line)
        {
            if (frames.size() == max_nframes)
            {
                incr_dropped_frames();
                return;
            }
            push_frame(intern_string(function_name), intern_string(file_name), line);
        }


        void push_alloc(const size_t size, const size_t count)
        {
            values.alloc_space += size;
            values.alloc_count += count;
        }

        void push_heap(const size_t size, const size_t count)
        {
            values.heap_space += size;
            values.heap_count += count;
        }

        void reset_alloc()
        {
            values.alloc_space = 0;
            values.alloc_count = 0;
        }

        /* Sampling-scaled estimate of how many allocations this sample stands
         * for, as computed by push_alloc. Read before reset_alloc to carry the
         * same estimate over to the heap (inuse) values. */
        size_t alloc_count() const
        {
            return values.alloc_count;
        }

        void clear()
        {
            values.alloc_space = 0;
            values.alloc_count = 0;
            values.heap_space = 0;
            values.heap_count = 0;
            frames.clear();
        }

        void export_sample() const
        {
            pyroscope_memprof_push_sample(FFISample{
                .frames = frames.data(),
                .len = frames.size(),
                .values = values,
            });
        }

        void push_threadinfo([[maybe_unused]] int64_t thread_id,
                             [[maybe_unused]] int64_t thread_native_id,
                             [[maybe_unused]] const char* name)
        {
            // no-op
        }

        void incr_dropped_frames()
        {
            // no-op
        }

        /* Stats sink for the vendored stack sampler; see ProfilerStats. */
        static ProfileBorrow profile_borrow()
        {
            return ProfileBorrow{};
        }
    };
}
