//! The string table and profile builder the profiler pushes samples into.
//!
//! These were two separate `lazy_static` mutexes. They are merged for two
//! reasons.
//!
//! **One lock acquisition per sample.** Interning used to take the string
//! table lock once per frame, up to 600 times for one sample, and then the
//! builder lock once more. A hook holds the GIL, so blocking on either stalls
//! the whole interpreter; one lock also removes any question of ordering
//! between them.
//!
//! **Fork safety.** They are now behind a [`LeakableMutex`], so a forked child
//! abandons the inherited mutex instead of trying to lock it. That fixes a
//! real hazard rather than a theoretical one: the previous code recovered from
//! mutex *poisoning*, but a mutex still held by a thread that no longer exists
//! in the child is a deadlock, not a poisoned lock, and the post-fork handler
//! locks it. It only failed to bite because every holder also held the GIL,
//! which the forking thread owns -- incidental, and it would break as soon as
//! any of this work moved off the GIL.

use crate::encode::pprof::sample::{Frame, HeapValues};
use crate::encode::pprof::{PProfBuilder, StringTable};
use crate::forksafety::LeakableMutex;
use crate::memalloc::pure::heap::Samples;
use std::sync::{MutexGuard, PoisonError};

/// Interned strings plus the accumulating profile.
#[derive(Default)]
pub struct MemSink {
    pub strings: StringTable,
    pub builder: PProfBuilder,
}

impl MemSink {
    /// Discard all interned strings and buffered samples.
    ///
    /// Called from `stop()` once the allocator hooks are uninstalled. Without
    /// it, samples buffered by a stopped session would leak into the next
    /// session's first profile and the string table would grow for the life of
    /// the process.
    ///
    /// Ordering matters: the heap tracker must be torn down *before* this
    /// runs, because its live samples hold interned string IDs that would
    /// otherwise dangle.
    pub fn reset(&mut self) {
        self.strings = StringTable::new();
        self.builder.reset();
    }
}

/// The process-wide sink.
static SINK: LeakableMutex<MemSink> = LeakableMutex::new();

/// Lock the sink, recovering from poisoning.
///
/// Poisoning is not interesting here: the contents are a string table and a
/// sample accumulator, both perfectly usable after an unrelated panic.
pub fn lock() -> MutexGuard<'static, MemSink> {
    SINK.mutex().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Allocate the inner mutex ahead of time.
///
/// [`LeakableMutex::mutex`] allocates on first use, so call this from
/// `start()` to keep that off the allocator-hook path.
pub fn prewarm() {
    let _ = SINK.mutex();
}

/// Abandon the sink inherited from the parent after a fork.
#[cfg(not(miri))]
pub fn leak_for_fork_child() {
    SINK.leak_and_reset();
}

/// Miri-only variant, handing back the abandoned allocation so the leak
/// checker stays quiet.
#[cfg(miri)]
#[must_use = "reclaim the returned allocation or Miri reports a leak"]
pub fn leak_for_fork_child() -> Option<std::ptr::NonNull<std::sync::Mutex<MemSink>>> {
    SINK.leak_and_reset()
}

/// A [`Samples`] implementation that walks the real interpreter and pushes
/// into the sink.
///
/// Holds the lock for the whole of one sample, so `collect` can intern frame
/// names and `emit` can fold the sample in without releasing and reacquiring.
pub struct SinkSamples<'a, C> {
    guard: &'a mut MemSink,
    collector: C,
}

impl<'a, C> SinkSamples<'a, C> {
    /// Wrap a locked sink and a stack collector.
    pub fn new(guard: &'a mut MemSink, collector: C) -> Self {
        Self { guard, collector }
    }
}

/// How a [`SinkSamples`] obtains the current Python stack.
///
/// Separate from [`Samples`] so the interpreter-touching part is the only
/// thing that has to be swapped out in tests.
pub trait StackCollector {
    /// Append the current Python stack to `frames`, interning through
    /// `strings`.
    fn collect(&mut self, strings: &mut StringTable, max_nframe: u16, frames: &mut Vec<Frame>);
}

impl<C: StackCollector> Samples for SinkSamples<'_, C> {
    fn collect(&mut self, max_nframe: u16, frames: &mut Vec<Frame>) {
        self.collector
            .collect(&mut self.guard.strings, max_nframe, frames);
    }

    fn emit(&mut self, frames: &[Frame], values: HeapValues) {
        if frames.is_empty() {
            return;
        }
        self.guard.builder.add_memory_sample(frames, &values);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::arithmetic_side_effects
    )]

    use super::*;
    use crate::encode::pprof::sample::Interned;
    use crate::memalloc::pure::heap::HeapTracker;

    /// An arbitrary window; the sink does not care what it is.
    fn test_range() -> crate::utils::TimeRange {
        let start = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        crate::utils::TimeRange::new(start, start + std::time::Duration::from_secs(10))
            .expect("a fixed range after the epoch is valid")
    }

    /// Produces fixed frame names, so the sink can be driven without an
    /// interpreter.
    struct FakeCollector {
        names: Vec<&'static str>,
    }

    impl StackCollector for FakeCollector {
        fn collect(&mut self, strings: &mut StringTable, max_nframe: u16, frames: &mut Vec<Frame>) {
            for name in self.names.iter().take(usize::from(max_nframe)) {
                let function_name = (&strings.add(name)).into();
                let file_name = (&strings.add("fake.py")).into();
                frames.push(Frame {
                    function_name,
                    file_name,
                    line: 1,
                });
            }
        }
    }

    #[test]
    fn prewarming_then_locking_works() {
        prewarm();
        let mut guard = lock();
        guard.reset();
    }

    /// The tracker and sink must compose: samples pushed through the sink end
    /// up in a profile that can be taken.
    #[test]
    fn a_tracked_allocation_reaches_the_builder() {
        let mut sink = MemSink::default();
        let mut tracker = HeapTracker::with_seed(1, 8, 1);

        {
            let collector = FakeCollector {
                names: vec!["outer", "inner"],
            };
            let mut samples = SinkSamples::new(&mut sink, collector);
            tracker.track(&mut samples, 4096, 64);
            tracker.flush(&mut samples);
        }

        assert_eq!(tracker.live(), 1);
        // The interned names must be present in the string table.
        let profile = sink
            .builder
            .take_profile_and_reset(&sink.strings, &test_range());
        assert!(profile.is_some(), "no profile was produced");
    }

    #[test]
    fn an_empty_frame_list_is_not_emitted() {
        let mut sink = MemSink::default();
        {
            let collector = FakeCollector { names: vec![] };
            let mut samples = SinkSamples::new(&mut sink, collector);
            samples.emit(&[], HeapValues::default());
        }
        let profile = sink
            .builder
            .take_profile_and_reset(&sink.strings, &test_range());
        assert!(profile.is_none(), "an empty sample was recorded");
    }

    #[test]
    fn reset_clears_the_sink() {
        let mut sink = MemSink::default();
        let id = sink.strings.add("something");
        assert_ne!(
            id.pprof(),
            0,
            "the first string should not be the empty one"
        );
        sink.reset();
        // A fresh table hands out the same first index again.
        let again = sink.strings.add("something else");
        assert_eq!(again.pprof(), id.pprof());
    }

    #[test]
    fn unused_interned_string_type_compiles() {
        let _ = Interned::default();
    }
}
