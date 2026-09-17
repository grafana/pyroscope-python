//
// Created by korniltsev on 2/6/2026.
//
#pragma once

#include <cstdint>
#include <vector>


extern "C" {
#include "pyroscope_ffi.h"
}

namespace Pyroscope
{
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


        /* Hand Rust a writable span and let it fill in the frames.
         *
         * Rust walks the frame chain and interns the strings, so this side no
         * longer needs to know anything about CPython's internals. The vector
         * is sized to the frame cap up front and then truncated to what was
         * actually written, so no allocation happens after the first sample
         * reuses a pooled traceback. */
        void collect_frames(const uint16_t max_nframe)
        {
            frames.resize(max_nframes);
            const uintptr_t written =
                pyroscope_memprof_collect_stack(max_nframe, frames.data(), frames.size());
            frames.resize(written);
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

    };
}
