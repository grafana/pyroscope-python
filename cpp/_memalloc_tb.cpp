#include "_memalloc_tb.h"

void
traceback_t::init_sample(size_t size, size_t weighted_size, uint16_t max_nframe)
{
    // Size 0 allocations are legal and we can hypothetically sample them,
    // e.g. if an allocation during sampling pushes us over the next sampling threshold,
    // but we can't sample it, so we sample the next allocation which happens to be 0
    // bytes. Defensively make sure size isn't 0.
    size_t adjusted_size = size > 0 ? size : 1;
    double scaled_count = ((double)weighted_size) / ((double)adjusted_size);
    size_t count = (size_t)scaled_count;

    sample.push_alloc(weighted_size, count);

    // Pyroscope patch: frame collection moved to Rust. It walks the frame
    // chain through the offsets CPython publishes in _Py_DebugOffsets, with
    // the same no-refcount, no-allocation constraints the C++ walker had, and
    // interns the strings itself. See rust/src/memalloc/pure/frames.rs.
    //
    // Thread info is not collected: Pyroscope::Sample::push_threadinfo was
    // always a no-op, so the CPython calls that fed it were pure overhead
    // inside the allocator hook.
    sample.collect_frames(max_nframe);
}

// AIDEV-NOTE: Constructor calls init_sample() which collects the Python stack
// Pyroscope patch: its sample adapter only needs the frame limit; Datadog
// sample-type flags do not apply to the Rust profile builder.
traceback_t::traceback_t(size_t size, size_t weighted_size, uint16_t max_nframe)
  : sample(max_nframe)
{
    if (max_nframe == 0) {
        return;
    }

    init_sample(size, weighted_size, max_nframe);
}
