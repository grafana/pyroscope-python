//! Test-only instrumentation. Never compiled into the shipped extension.
//!
//! Hosts the counting global allocator used to prove that the reentrancy
//! guard's thread-local access does not allocate. This is the *one* permitted
//! `#[global_allocator]` in the crate, and it is `#[cfg(test)]`: routing
//! Rust's allocator anywhere near PyMem in a real build would make the hook
//! path infinitely recursive (see the module docs in `super`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Allocations observed on the armed thread.
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// `pthread_self()` of the thread being measured, or 0 when disarmed.
///
/// Scoping by thread is what makes the measurement trustworthy: `cargo test`
/// runs tests in parallel, so a plain global counter would pick up unrelated
/// allocations from other tests.
static ARMED: AtomicU64 = AtomicU64::new(0);

fn current_thread() -> u64 {
    // `pthread_self` is a register read; it cannot allocate, which is what
    // makes it safe to call from inside the allocator.
    unsafe { libc::pthread_self() as u64 }
}

#[inline]
fn record() {
    if ARMED.load(Ordering::Relaxed) == current_thread() {
        COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Delegates to the system allocator, counting traffic on the armed thread.
pub struct CountingAllocator;

// SAFETY: every method forwards to `System` with the same layout, adding only
// atomic bookkeeping.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        unsafe { System.alloc_zeroed(layout) }
    }
}

/// Count allocations made by `f` on this thread.
///
/// Serialised on a mutex so two concurrent tests cannot arm at once. `f`
/// should avoid printing or formatting, both of which allocate.
pub fn count_allocations<R>(f: impl FnOnce() -> R) -> (R, usize) {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());

    let _held = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    ARMED.store(current_thread(), Ordering::SeqCst);
    COUNT.store(0, Ordering::SeqCst);
    let out = f();
    let observed = COUNT.load(Ordering::SeqCst);
    ARMED.store(0, Ordering::SeqCst);
    (out, observed)
}
