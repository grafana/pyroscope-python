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

        void push_heap(const size_t size)
        {
            values.heap_space += size;
        }

        void reset_alloc()
        {
            values.alloc_space = 0;
            values.alloc_count = 0;
        }

        void clear()
        {
            values.alloc_space = 0;
            values.alloc_count = 0;
            values.heap_space = 0;
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
