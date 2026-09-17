//! The process-wide heap tracker, and the entry points the allocator hooks
//! call.
//!
//! # Why an `AtomicPtr` and not a `Mutex`
//!
//! The tracker is only ever touched from an allocator hook, which always holds
//! the GIL, so the GIL already serialises it -- exactly as in the C++, where
//! the tracker was a bare `heap_tracker_t*`.
//!
//! A mutex would be worse than redundant. It would add an atomic round trip to
//! the hottest path in the process, and it would introduce a self-deadlock
//! hazard: if anything on the sampling path ever allocated through PyMem,
//! re-entering while the guard was alive would hang the interpreter with the
//! GIL held. Publishing through [`LeakablePtr`] makes that same mistake
//! degrade to a skipped sample instead, because the reentrancy guard turns it
//! away first.
//!
//! The "GIL is held" invariant is load-bearing, so debug builds assert it on
//! every access and count hooks in flight, which `deinit` then checks.

use crate::encode::pprof::StringTable;
use crate::encode::pprof::sample::Frame;
use crate::forksafety::LeakablePtr;
use crate::memalloc::pure::frames::{FrameSink, walk_frames};
use crate::memalloc::pure::heap::HeapTracker;
use crate::memalloc::pure::offsets::Offsets;
use crate::memalloc::reentrancy;
use crate::memalloc::runtime::pyapi;
use crate::memalloc::runtime::reader::InProcess;
use crate::memalloc::sink::{self, SinkSamples, StackCollector};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The tracker, published by `init` and abandoned by the fork-child handler.
static TRACKER: LeakablePtr<HeapTracker> = LeakablePtr::new();

/// Hooks currently executing, in debug builds only.
///
/// `deinit` asserts this is zero, which is what makes the "our hooks never
/// release the GIL, so none can be in flight when we tear down" claim checked
/// rather than merely argued.
#[cfg(debug_assertions)]
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Samples skipped because the hook re-entered on this thread.
///
/// Surfaced so a nonzero value is visible in the field, not just in a debug
/// build.
static REENTRANT_SKIPS: AtomicUsize = AtomicUsize::new(0);

/// Run `f` against the tracker, if one is published.
///
/// # Safety
///
/// The caller must hold the GIL and must have acquired the reentrancy guard,
/// which together guarantee exclusive access.
unsafe fn with_tracker<R>(f: impl FnOnce(&mut HeapTracker) -> R) -> Option<R> {
    let tracker = TRACKER.load(Ordering::Acquire);
    if tracker.is_null() {
        return None;
    }
    debug_assert!(
        pyapi::gil_held(),
        "the heap tracker was touched without the GIL"
    );
    // SAFETY: the pointer came from `Box::into_raw` in `init` and is only
    // cleared by `deinit` or the fork handler, both of which run with the GIL
    // held. The caller's guarantees make this the only live reference.
    Some(f(unsafe { &mut *tracker }))
}

/// Walks the live interpreter to collect a stack.
struct InterpreterStack {
    offsets: Offsets,
}

impl StackCollector for InterpreterStack {
    fn collect(&mut self, strings: &mut StringTable, max_nframe: u16, frames: &mut Vec<Frame>) {
        let mut sink = InterningFrames { frames, strings };
        walk_frames(
            &InProcess,
            &self.offsets,
            &pyapi::type_addrs(),
            pyapi::current_thread_state(),
            max_nframe,
            &mut sink,
        );
    }
}

/// Interns each frame's strings and appends it to a buffer.
struct InterningFrames<'a> {
    frames: &'a mut Vec<Frame>,
    strings: &'a mut StringTable,
}

impl FrameSink for InterningFrames<'_> {
    fn push_frame(&mut self, function: &str, file: &str, line: i32) {
        let function_name = (&self.strings.add(function)).into();
        let file_name = (&self.strings.add(file)).into();
        self.frames.push(Frame {
            function_name,
            file_name,
            line,
        });
    }

    fn note_dropped(&mut self) {
        // The pprof sample has no field for this.
    }
}

/// Guard that counts a hook as in flight, in debug builds.
struct InFlight;

impl InFlight {
    fn enter() -> Self {
        #[cfg(debug_assertions)]
        IN_FLIGHT.fetch_add(1, Ordering::Acquire);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        IN_FLIGHT.fetch_sub(1, Ordering::Release);
    }
}

/// Create the tracker.
///
/// Returns false if one is already published, matching the C++, which refused
/// to reinitialise.
///
/// # Safety
///
/// Must be called with the GIL held and before any hook is installed.
pub fn heap_init(sample_size: u32, max_nframe: u16) -> bool {
    if !TRACKER.load(Ordering::Acquire).is_null() {
        return false;
    }
    // Allocate the sink's mutex and touch the reentrancy thread-local now, so
    // neither can allocate on the hook path later.
    sink::prewarm();
    reentrancy::touch();

    let previous = TRACKER.publish(Box::new(HeapTracker::new(sample_size, max_nframe)));
    debug_assert!(previous.is_null(), "raced with another init");
    true
}

/// Destroy the tracker and its live samples.
///
/// Must run *before* the sink is reset: live samples hold interned string IDs
/// which would otherwise dangle.
///
/// # Safety
///
/// Must be called with the GIL held and after the hooks are uninstalled.
pub fn heap_deinit() {
    #[cfg(debug_assertions)]
    debug_assert_eq!(
        IN_FLIGHT.load(Ordering::Acquire),
        0,
        "a hook was still in flight at teardown"
    );

    let tracker = TRACKER.take();
    if tracker.is_null() {
        return;
    }
    // SAFETY: the pointer came from `Box::into_raw` in `init`, and `take`
    // transferred ownership back to us, so nothing else can reach it.
    drop(unsafe { Box::from_raw(tracker) });
}

/// Account for an allocation, sampling it if the sampler says so.
///
/// # Safety
///
/// Called from inside the allocator hook with the GIL held. Must not allocate
/// through PyMem, touch refcounts, touch `PyErr`, or unwind.
pub fn heap_track(ptr: *mut std::ffi::c_void, size: usize) {
    if ptr.is_null() {
        return;
    }
    let _in_flight = InFlight::enter();

    // Take the guard before anything else, and hold it for the whole body.
    // The C++ tested the sampler *before* taking its guard, so a reentrant
    // allocation still advanced the byte counter and was double counted.
    let guard = reentrancy::Guard::acquire();
    if !guard.acquired() {
        REENTRANT_SKIPS.fetch_add(1, Ordering::Relaxed);
        return;
    }

    let Ok(offsets) = pyapi::resolve() else {
        return;
    };

    // SAFETY: the caller holds the GIL and we hold the reentrancy guard.
    unsafe {
        with_tracker(|tracker| {
            let mut guard = sink::lock();
            let mut samples = SinkSamples::new(&mut guard, InterpreterStack { offsets });
            tracker.track(&mut samples, ptr as usize, size);
        });
    }
}

/// Forget an allocation.
///
/// Deliberately *not* guarded against reentrancy: skipping an untrack would
/// leak a tracker entry forever, so freeing is always allowed to proceed. This
/// only touches the map and never calls into CPython, mirroring the C++, which
/// likewise left its free path unguarded.
///
/// # Safety
///
/// Called from inside the allocator hook with the GIL held.
pub fn heap_untrack(ptr: *mut std::ffi::c_void) {
    if ptr.is_null() {
        return;
    }
    let _in_flight = InFlight::enter();
    // SAFETY: the caller holds the GIL, which serialises access to the map.
    unsafe {
        with_tracker(|tracker| {
            tracker.untrack(ptr as usize);
        });
    }
}

/// Export every live sampled allocation into the profile.
///
/// # Safety
///
/// Must be called with the GIL held.
pub fn heap_flush() {
    let Ok(offsets) = pyapi::resolve() else {
        return;
    };
    // Flushing does not walk the stack, but `SinkSamples` needs a collector;
    // the tracker only calls `emit` from `flush`.
    // SAFETY: the caller holds the GIL.
    unsafe {
        with_tracker(|tracker| {
            let mut guard = sink::lock();
            let mut samples = SinkSamples::new(&mut guard, InterpreterStack { offsets });
            tracker.flush(&mut samples);
        });
    }
}

/// Abandon all state inherited from the parent after a fork.
///
/// Leaks rather than drops: freeing memory inherited from a parent through
/// libc is unsafe in a `fork()`-without-`exec()` child on macOS, and the child
/// must not report the parent's allocations either way.
///
/// # Safety
///
/// Must be called from a post-fork child handler, before any other Python code
/// runs.
pub fn heap_postfork_child() {
    // Pin this thread as "inside a hook" so nothing the handler itself
    // allocates gets tracked while we are tearing state down.
    reentrancy::force_set(true);

    #[cfg(not(miri))]
    {
        TRACKER.leak();
        sink::leak_for_fork_child();
    }
    #[cfg(miri)]
    {
        // Reclaim what was abandoned so Miri's leak checker stays quiet.
        if let Some(tracker) = TRACKER.leak() {
            // SAFETY: `leak` handed back the allocation it abandoned.
            drop(unsafe { Box::from_raw(tracker.as_ptr()) });
        }
        if let Some(sink) = sink::leak_for_fork_child() {
            // SAFETY: as above.
            drop(unsafe { Box::from_raw(sink.as_ptr()) });
        }
    }

    reentrancy::clear_for_fork_child();
}

/// Samples skipped because the hook re-entered, for diagnostics.
pub fn reentrant_skips() -> usize {
    REENTRANT_SKIPS.load(Ordering::Relaxed)
}
