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


        void push_frame(const std::string_view function_name, const std::string_view file_name, int _, const int line)
        {
            if (frames.size() == max_nframes)
            {
                incr_dropped_frames();
            }
            else
            {
                frames.emplace_back(
                    FFIFrame{
                        .function_name = intern_string(function_name),
                        .file_name = intern_string(file_name),
                        .line = line,
                    }
                );
            }
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

    private:
        static FFIInternedString intern_string(std::string_view s)
        {
            return pyroscope_memprof_string_table_intern_string(FFIStringView{
                .data = s.data(),
                .len = s.length()
            });
        }
    };
}
