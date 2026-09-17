//! Fixed limits carried over from the C++ profiler.
//!
//! These are deliberately identical to the C++ values so that profiles do not
//! change shape across the rewrite.

/// Maximum number of frames emitted for one sample.
///
/// Bounded by the maximum frame count the backend accepts.
/// (`TRACEBACK_MAX_NFRAME` in `cpp/_memalloc_tb.h`.)
pub const TRACEBACK_MAX_NFRAME: u16 = 600;

/// Hard cap on raw frame-chain traversal.
///
/// Deliberately separate from [`TRACEBACK_MAX_NFRAME`]: skipped or malformed
/// frames must not be able to leave the walk effectively unbounded, so the
/// number of links followed is capped independently of the number emitted.
/// (`TRACEBACK_MAX_WALKED_NFRAME` in `cpp/_memalloc_tb.h`.)
pub const TRACEBACK_MAX_WALKED_NFRAME: u32 = 1024;

/// Maximum number of live sampled allocations tracked at once.
///
/// Inherited from the original array-based implementation. It bounds memory
/// use, but once the limit is hit the reported numbers become inaccurate.
/// (`TRACEBACK_ARRAY_MAX_COUNT` in `cpp/_memalloc_tb.h`.)
pub const TRACEBACK_ARRAY_MAX_COUNT: usize = u16::MAX as usize;

/// Largest accepted heap sampling interval, in bytes.
///
/// (`MAX_HEAP_SAMPLE_SIZE` in `cpp/_memalloc_heap.h`.)
pub const MAX_HEAP_SAMPLE_SIZE: u64 = u32::MAX as u64;

/// Capacity of the recycling pool for per-sample buffers.
///
/// (`heap_tracker_t::POOL_CAPACITY` in `cpp/_memalloc_heap.cpp`.)
pub const POOL_CAPACITY: usize = 128;

/// Initial capacity of the live-allocation map, to avoid rehashing during
/// ramp-up.
///
/// (`heap_tracker_t::INITAL_ALLOC_MAP_CAPACITY` in `cpp/_memalloc_heap.cpp`.)
pub const INITIAL_ALLOC_MAP_CAPACITY: usize = 512;
