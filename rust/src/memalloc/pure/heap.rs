//! Tracking live sampled allocations.
//!
//! Ported from `heap_tracker_t` in `cpp/_memalloc_heap.cpp`. Owns the sampling
//! state, the map of live sampled allocations, and a pool that recycles the
//! per-sample frame buffers.
//!
//! Sample collection and export are abstracted behind [`Samples`], so the
//! whole state machine runs in tests against a stub, in the
//! `--no-default-features` build, and therefore under Miri. That is the only
//! automated check on this logic: the real thing runs inside CPython's
//! allocator and cannot be driven from `cargo test`.
//!
//! # Weighting
//!
//! Each sample stands for roughly one sampling interval of bytes rather than
//! its own size, so the weight is the byte count since the previous sample
//! (see [`crate::memalloc::pure::sampler`]). The allocation sample is exported
//! immediately and not retained; only the live values stay behind, carrying
//! the *same* scaled object count, so `inuse_objects` is derived the same way
//! `alloc_objects` was. The C++ achieved that by reading the count before
//! clearing the allocation values, and getting it wrong would silently change
//! what `inuse_objects` means.

use crate::encode::pprof::ffi::{FFIFrame, FFIHeapSampleValues};
use crate::memalloc::limits::{
    INITIAL_ALLOC_MAP_CAPACITY, POOL_CAPACITY, TRACEBACK_ARRAY_MAX_COUNT,
};
use crate::memalloc::pure::sampler::{Sampler, scaled_count};

/// Collecting and exporting samples.
///
/// Implemented for real by the profiler's sink, which walks the Python stack
/// and pushes into the pprof builder, and by stubs in tests.
pub trait Samples {
    /// Fill `frames` with the current Python stack, innermost first.
    ///
    /// The buffer arrives cleared and may already have capacity from the pool.
    fn collect(&mut self, max_nframe: u16, frames: &mut Vec<FFIFrame>);

    /// Export one sample.
    fn emit(&mut self, frames: &[FFIFrame], values: FFIHeapSampleValues);
}

/// A sampled allocation that is still live.
struct LiveSample {
    frames: Vec<FFIFrame>,
    heap_space: usize,
    heap_count: usize,
}

/// Sampling state plus the live-allocation map.
///
/// Not internally synchronised: like the C++ it relies on the GIL, which the
/// allocator hook always holds. See `runtime::heap` for how that invariant is
/// upheld and checked.
pub struct HeapTracker {
    sampler: Sampler,
    max_nframe: u16,
    /// Live sampled allocations, keyed by address.
    allocs: hashbrown::HashMap<usize, LiveSample>,
    /// Recycled frame buffers, so a steady state does no allocation.
    pool: Vec<Vec<FFIFrame>>,
    /// Samples declined because the live map was full.
    declined_map_full: u64,
    /// Addresses that were already tracked when inserted, which means an
    /// untrack was missed somewhere.
    duplicate_inserts: u64,
}

impl HeapTracker {
    /// Create a tracker with the given sampling interval and frame cap.
    pub fn new(sample_size: u32, max_nframe: u16) -> Self {
        Self::with_seed(sample_size, max_nframe, sample_size)
    }

    /// Create a tracker with an explicit RNG seed, for deterministic tests.
    pub fn with_seed(sample_size: u32, max_nframe: u16, seed: u32) -> Self {
        let mut allocs = hashbrown::HashMap::new();
        // Pre-size to avoid rehashing during ramp-up, as the C++ did.
        allocs.reserve(INITIAL_ALLOC_MAP_CAPACITY);
        Self {
            sampler: Sampler::with_seed(sample_size, seed),
            max_nframe,
            allocs,
            pool: Vec::with_capacity(POOL_CAPACITY),
            declined_map_full: 0,
            duplicate_inserts: 0,
        }
    }

    /// Account for an allocation of `size` bytes at `ptr`, sampling it if the
    /// sampler says so.
    pub fn track<S: Samples>(&mut self, samples: &mut S, ptr: usize, size: usize) {
        let Some(weight) = self.sampler.on_alloc(size) else {
            return;
        };

        // Bounded memory use, inherited from the original array-based
        // implementation. Note this deliberately does *not* reset the
        // sampler: the C++ left the byte counter running so that sampling
        // resumes as soon as space frees up.
        if self.allocs.len() > TRACEBACK_ARRAY_MAX_COUNT {
            self.declined_map_full = self.declined_map_full.saturating_add(1);
            return;
        }

        let mut frames = self.pool.pop().unwrap_or_default();
        frames.clear();
        samples.collect(self.max_nframe, &mut frames);

        // PANIC-OK: `weight` is bounded by the sampler and `usize` is 64-bit
        // on every platform we ship.
        #[allow(clippy::cast_possible_truncation)]
        let space = weight as usize;
        #[allow(clippy::cast_possible_truncation)]
        let count = scaled_count(size, weight) as usize;

        // The allocation sample is exported now and not retained.
        samples.emit(
            &frames,
            FFIHeapSampleValues {
                alloc_space: space,
                alloc_count: count,
                heap_space: 0,
                heap_count: 0,
            },
        );

        // The live values carry the same scaled count, so inuse_objects means
        // the same thing alloc_objects did.
        let replaced = self.allocs.insert(
            ptr,
            LiveSample {
                frames,
                heap_space: space,
                heap_count: count,
            },
        );
        if let Some(old) = replaced {
            // Should be unreachable: an address cannot be handed out twice
            // without an intervening free. If it happens we leaked an entry.
            self.duplicate_inserts = self.duplicate_inserts.saturating_add(1);
            self.recycle(old.frames);
        }

        self.sampler.reset();
    }

    /// Forget the allocation at `ptr`, if it was sampled.
    pub fn untrack(&mut self, ptr: usize) {
        if let Some(entry) = self.allocs.remove(&ptr) {
            self.recycle(entry.frames);
        }
    }

    /// Export every live sampled allocation.
    pub fn flush<S: Samples>(&self, samples: &mut S) {
        for entry in self.allocs.values() {
            samples.emit(
                &entry.frames,
                FFIHeapSampleValues {
                    heap_space: entry.heap_space,
                    heap_count: entry.heap_count,
                    alloc_space: 0,
                    alloc_count: 0,
                },
            );
        }
    }

    /// Drop all inherited state in a forked child.
    ///
    /// The child must not report the parent's allocations, and must not run
    /// profiler code against a half-updated map. Called before any Python
    /// code runs in the child.
    pub fn postfork_child(&mut self) {
        self.pool.clear();
        self.allocs.clear();
        self.sampler.reset();
    }

    /// Number of live sampled allocations.
    pub fn live(&self) -> usize {
        self.allocs.len()
    }

    /// Number of buffers currently pooled.
    pub fn pooled(&self) -> usize {
        self.pool.len()
    }

    /// Counters worth surfacing in a dump: samples declined because the map
    /// was full, and addresses inserted twice.
    pub fn counters(&self) -> (u64, u64) {
        (self.declined_map_full, self.duplicate_inserts)
    }

    fn recycle(&mut self, mut frames: Vec<FFIFrame>) {
        // Clear before pooling so a retained buffer never keeps stale frames
        // alive, and keep the pool bounded.
        frames.clear();
        if self.pool.len() < POOL_CAPACITY {
            self.pool.push(frames);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::panic
    )]

    use super::*;
    use crate::encode::pprof::ffi::FFIInternedString;
    use crate::memalloc::pure::rng::MinstdRand;

    /// Records what the tracker asked for, with no interpreter involved.
    #[derive(Default)]
    struct StubSamples {
        /// Every emitted sample, as `(frame count, values)`.
        emitted: Vec<(usize, FFIHeapSampleValues)>,
        /// How many frames `collect` should produce.
        frames_to_make: usize,
        /// Frame-cap values `collect` was asked for.
        caps_seen: Vec<u16>,
    }

    impl Samples for StubSamples {
        fn collect(&mut self, max_nframe: u16, frames: &mut Vec<FFIFrame>) {
            self.caps_seen.push(max_nframe);
            assert!(frames.is_empty(), "buffer arrived dirty");
            for i in 0..self.frames_to_make {
                frames.push(FFIFrame {
                    function_name: FFIInternedString { index: i as u32 },
                    file_name: FFIInternedString { index: 0 },
                    line: i as i32,
                });
            }
        }

        fn emit(&mut self, frames: &[FFIFrame], values: FFIHeapSampleValues) {
            self.emitted.push((frames.len(), values));
        }
    }

    impl StubSamples {
        fn allocs(&self) -> Vec<&FFIHeapSampleValues> {
            self.emitted
                .iter()
                .filter(|(_, v)| v.alloc_space > 0 || v.alloc_count > 0)
                .map(|(_, v)| v)
                .collect()
        }

        fn heaps(&self) -> Vec<&FFIHeapSampleValues> {
            self.emitted
                .iter()
                .filter(|(_, v)| v.heap_space > 0 || v.heap_count > 0)
                .map(|(_, v)| v)
                .collect()
        }
    }

    /// Feed allocations until one is sampled, returning its address.
    fn track_until_sampled(
        tracker: &mut HeapTracker,
        stub: &mut StubSamples,
        size: usize,
    ) -> usize {
        for address in 1..1_000_000usize {
            let before = stub.emitted.len();
            tracker.track(stub, address * 64, size);
            if stub.emitted.len() > before {
                return address * 64;
            }
        }
        panic!("no allocation was ever sampled");
    }

    #[test]
    fn a_sampled_allocation_is_exported_once_and_retained_once() {
        let mut tracker = HeapTracker::with_seed(1024, 64, 1);
        let mut stub = StubSamples {
            frames_to_make: 3,
            ..Default::default()
        };
        let ptr = track_until_sampled(&mut tracker, &mut stub, 512);

        assert_eq!(stub.allocs().len(), 1, "one allocation sample");
        assert_eq!(stub.heaps().len(), 0, "live values are not exported yet");
        assert_eq!(tracker.live(), 1);
        assert_eq!(stub.emitted[0].0, 3, "frames were not passed through");

        tracker.flush(&mut stub);
        assert_eq!(stub.heaps().len(), 1, "flush exports the live sample");
        assert_eq!(tracker.live(), 1, "flush must not consume the map");

        tracker.untrack(ptr);
        assert_eq!(tracker.live(), 0);
    }

    /// The property that gives `inuse_objects` its meaning: the retained
    /// count is the same estimate the allocation sample used.
    #[test]
    fn live_values_reuse_the_allocation_estimate() {
        let mut tracker = HeapTracker::with_seed(4096, 64, 5);
        let mut stub = StubSamples::default();
        track_until_sampled(&mut tracker, &mut stub, 64);
        tracker.flush(&mut stub);

        let alloc = *stub.allocs()[0];
        let heap = *stub.heaps()[0];
        assert_eq!(heap.heap_space, alloc.alloc_space, "space differs");
        assert_eq!(heap.heap_count, alloc.alloc_count, "count differs");
        // And the two kinds never overlap in one sample.
        assert_eq!(alloc.heap_space, 0);
        assert_eq!(heap.alloc_space, 0);
    }

    #[test]
    fn the_frame_cap_is_passed_through() {
        let mut tracker = HeapTracker::with_seed(64, 17, 3);
        let mut stub = StubSamples::default();
        track_until_sampled(&mut tracker, &mut stub, 64);
        assert_eq!(stub.caps_seen, [17]);
    }

    #[test]
    fn untracking_an_unknown_address_is_a_no_op() {
        let mut tracker = HeapTracker::with_seed(64, 8, 1);
        tracker.untrack(0);
        tracker.untrack(0xdead_beef);
        assert_eq!(tracker.live(), 0);
        assert_eq!(tracker.pooled(), 0);
    }

    /// Buffers must be recycled, and the pool must stay bounded, or a
    /// long-running process either allocates forever or grows without limit.
    #[test]
    fn buffers_are_recycled_and_the_pool_stays_bounded() {
        let mut tracker = HeapTracker::with_seed(1, 8, 9);
        let mut stub = StubSamples {
            frames_to_make: 4,
            ..Default::default()
        };

        // Sample size 1 means almost everything is sampled.
        let mut live = Vec::new();
        for i in 1..=(POOL_CAPACITY * 3) {
            let ptr = i * 64;
            tracker.track(&mut stub, ptr, 64);
            live.push(ptr);
        }
        for ptr in &live {
            tracker.untrack(*ptr);
        }

        assert_eq!(tracker.live(), 0, "entries were not removed");
        assert!(
            tracker.pooled() <= POOL_CAPACITY,
            "pool grew to {} past the {POOL_CAPACITY} cap",
            tracker.pooled()
        );
    }

    /// Churn must return to a clean state, which is the leak canary for the
    /// map and the pool.
    #[test]
    fn heavy_churn_returns_to_empty() {
        let mut tracker = HeapTracker::with_seed(128, 16, 21);
        let mut stub = StubSamples::default();
        let mut rng = MinstdRand::new(7);

        let rounds = if cfg!(miri) { 200 } else { 20_000 };
        for i in 1..=rounds {
            let ptr = i * 32;
            tracker.track(&mut stub, ptr, (rng.next_u32() % 4096) as usize + 1);
            tracker.untrack(ptr);
        }

        assert_eq!(tracker.live(), 0);
        assert!(tracker.pooled() <= POOL_CAPACITY);
        let (declined, duplicates) = tracker.counters();
        assert_eq!(declined, 0);
        assert_eq!(duplicates, 0);
    }

    /// Reusing an address without an intervening untrack must replace the
    /// entry rather than leak it, and must be counted.
    #[test]
    fn a_duplicate_address_replaces_and_is_counted() {
        let mut tracker = HeapTracker::with_seed(1, 8, 4);
        let mut stub = StubSamples::default();
        tracker.track(&mut stub, 4096, 64);
        tracker.track(&mut stub, 4096, 64);

        assert_eq!(tracker.live(), 1, "the map should hold one entry");
        let (_, duplicates) = tracker.counters();
        assert_eq!(duplicates, 1, "the duplicate was not noticed");
    }

    #[test]
    fn a_forked_child_starts_clean() {
        let mut tracker = HeapTracker::with_seed(1, 8, 2);
        let mut stub = StubSamples::default();
        for i in 1..=10usize {
            tracker.track(&mut stub, i * 64, 64);
        }
        tracker.untrack(64);
        assert!(tracker.live() > 0);
        assert!(tracker.pooled() > 0);

        tracker.postfork_child();
        assert_eq!(tracker.live(), 0, "inherited allocations were kept");
        assert_eq!(tracker.pooled(), 0, "inherited buffers were kept");

        // And it still works afterwards.
        let before = stub.emitted.len();
        track_until_sampled(&mut tracker, &mut stub, 64);
        assert!(stub.emitted.len() > before);
    }

    #[test]
    fn flushing_an_empty_tracker_emits_nothing() {
        let tracker = HeapTracker::with_seed(1024, 8, 1);
        let mut stub = StubSamples::default();
        tracker.flush(&mut stub);
        assert!(stub.emitted.is_empty());
    }

    /// Total exported allocation space must track the bytes fed in, or every
    /// profile is scaled wrong while still looking plausible.
    #[test]
    fn exported_space_tracks_bytes_allocated() {
        let interval = 4096u32;
        let mut tracker = HeapTracker::with_seed(interval, 8, 0xBEEF);
        let mut stub = StubSamples::default();
        let mut rng = MinstdRand::new(11);

        let want_samples = if cfg!(miri) { 20u64 } else { 400 };
        let budget = u64::from(interval) * want_samples;
        let mut allocated = 0u64;
        let mut ptr = 4096usize;
        while allocated < budget {
            let size = (rng.next_u32() % 512) as usize + 1;
            allocated += size as u64;
            ptr += 64;
            tracker.track(&mut stub, ptr, size);
            tracker.untrack(ptr);
        }

        let exported: u64 = stub.allocs().iter().map(|v| v.alloc_space as u64).sum();
        let samples = stub.allocs().len() as f64;
        let ratio = exported as f64 / allocated as f64;
        let tolerance = (4.0 / samples.sqrt()).clamp(0.10, 1.5);
        assert!(
            (ratio - 1.0).abs() <= tolerance,
            "exported {exported} of {allocated} bytes over {samples} samples \
             (ratio {ratio:.4}, tolerance {tolerance:.4})"
        );
    }
}
