//
// Pyroscope sink shim for vendored CPU profilers.
//
// This is the CPU analogue of cpp/Pyroscope.h (which replaced Datadog's
// ddup_interface for the memalloc profiler). A vendored CPU profiler renders
// its samples through this class instead of through its upstream sink
// (Datadog's ddup_* / libdd_wrapper), so it feeds the same Rust encoder and
// uploader everything else in this extension uses.
//
// It lives one level above the individual profiler directories so a second
// vendored sampler can share it without duplicating the C ABI glue.
//
#pragma once

#include <cstdint>
#include <string_view>
#include <vector>

extern "C" {
#include "pyroscope_ffi.h"
}

namespace Pyroscope
{
    /* Accumulates one stack trace and hands it to the Rust sink.
     *
     * IMPORTANT: export_sample() crosses into Rust, where it allocates and
     * takes a lock. It must NEVER be called from a signal handler. The
     * vendored echion sampler already has the right shape for this -- its
     * signal handler only does siglongjmp recovery for safe_memcpy, and all
     * rendering happens on the ordinary sampling thread. Preserve that.
     *
     * Reuse one instance across samples and call clear() between them; the
     * frame vector keeps its capacity so the sampling loop does not allocate
     * per sample.
     */
    class CpuSample
    {
        std::vector<FFICpuFrame> frames;
        size_t max_nframes;
        uint64_t thread_id{0};
        std::string_view thread_name{};
        uint32_t pid{0};
        uint64_t cpu_nanos{0};
        uint64_t dropped_frames{0};

    public:
        explicit CpuSample(const size_t max_nframes) : max_nframes{max_nframes}
        {
            frames.reserve(max_nframes);
        }

        /* Frames must be pushed leaf-first, matching py-spy's ordering. */
        void push_frame(const std::string_view function_name,
                        const std::string_view file_name,
                        const int line)
        {
            if (frames.size() >= max_nframes)
            {
                dropped_frames++;
                return;
            }
            frames.emplace_back(
                FFICpuFrame{
                    .function_name = string_view(function_name),
                    .file_name = string_view(file_name),
                    .line = line,
                }
            );
        }

        void set_thread(const uint64_t tid, const std::string_view name)
        {
            thread_id = tid;
            thread_name = name;
        }

        void set_pid(const uint32_t p) { pid = p; }

        /* CPU nanoseconds this sample accounts for.
         *
         * Must be real CPU time, not wall time. A wall-clock sampler that
         * walks every Python thread sees one sample per thread per tick; if
         * each were credited a full sampling period of CPU, the profile would
         * claim several times more CPU than the process actually consumed and
         * would count blocked threads as busy. A sampler whose timer fires
         * once per period of CPU would pass ticks * period here. */
        void set_cpu_nanos(const uint64_t ns) { cpu_nanos = ns; }

        bool empty() const { return frames.empty(); }

        void clear()
        {
            frames.clear();
            thread_id = 0;
            thread_name = {};
            pid = 0;
            cpu_nanos = 0;
            dropped_frames = 0;
        }

        void export_sample() const
        {
            /* A sample with no CPU time is not a CPU sample. Filtering here
             * rather than in Rust keeps zero-valued entries out of the sink's
             * aggregation map entirely. */
            if (frames.empty() || cpu_nanos == 0)
            {
                return;
            }
            pyroscope_cpu_push_sample(FFICpuSample{
                .frames = frames.data(),
                .len = frames.size(),
                .pid = pid,
                .thread_id = thread_id,
                .thread_name = string_view(thread_name),
                .cpu_nanos = cpu_nanos,
            });
        }

    private:
        static FFIStringView string_view(const std::string_view s)
        {
            return FFIStringView{
                .data = s.data(),
                .len = s.length(),
            };
        }
    };
}
