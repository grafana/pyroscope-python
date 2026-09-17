#pragma once

#include <cstdint>
#include <string>
#include <string_view>

#include "python_headers.hpp"

#include "dd_wrapper/include/sample.hpp"

// Pyroscope patch: interned string ids come from Pyroscope's Rust-backed
// string table instead of libdatadog's Profiles Dictionary.
#include "Pyroscope.h"

#include "echion/frame.h"
#include "echion/timing.h"

namespace Datadog {

// Pyroscope patch: upstream gets this typedef from
// dd_wrapper/include/sample.hpp, where it is an opaque libdatadog handle
// (ddog_prof_StringId2). Pyroscope's is a { uint32_t index; } struct.
using string_id = Pyroscope::string_id;

struct ThreadState
{
    // Current thread info.  Keeping one instance of this per StackRenderer is sufficient because the renderer visits
    // threads one at a time.
    // The only time this information is revealed is when the sampler observes a thread. When the sampler goes on to
    // process tasks, it needs to place thread-level information in the Sample.
    uintptr_t id = 0;
    unsigned long native_id = 0;
    std::string name;
    microsecond_t wall_time_ns = 0;
    microsecond_t cpu_time_ns = 0;
    int64_t now_time_ns = 0;
};

class StackRenderer
{
    Sample* sample = nullptr;
    ThreadState thread_state = {};

    // Pyroscope patch: memoises echion's StringTable::Key -> Pyroscope::string_id
    // so a frame we have seen before costs one hash lookup instead of a call
    // across the FFI boundary. Upstream also cached function IDs here; we do
    // not, because Pyroscope has no function ids -- function and location
    // dedup is the Rust encoder's job (PProfBuilder::add_function_mirror).
    //
    // Keep this cache. It is load-bearing for us in a way it was not upstream:
    // libdatadog interns into a 16-way sharded set whose hit path takes only a
    // read lock, whereas our string table is a single mutex held exclusively
    // even on a hit. This cache is what keeps interning a once-per-unique-frame
    // cost rather than a global lock acquisition per frame on the sampling
    // thread.
    std::unordered_map<StringTable::Key, string_id> string_id_cache;

    // Whether task name has been pushed for the current sample. Whenever
    // the sample is created, this has to be reset.
    bool pushed_task_name = false;

  public:
    StackRenderer();
    void render_thread_begin(PyThreadState* tstate,
                             std::string_view name,
                             microsecond_t wall_time_us,
                             uintptr_t thread_id,
                             unsigned long native_id);
    void render_task_begin(const std::string& task_name, bool on_cpu);
    void render_frame(Frame& frame);
    void render_cpu_time(microsecond_t cpu_time_us);
    void render_native_frame(const std::string& name, const std::string& module);
    void render_stack_end();

    // Clear caches after fork to avoid using stale interned string/function IDs
    void postfork_child();
};

} // namespace Datadog
