//! Per-thread reentrancy guard for the allocator hooks.
//!
//! Replaces `_MEMALLOC_ON_THREAD` and `memalloc_reentrant_guard_t` from
//! `cpp/_memalloc_reentrant.{h,cpp}`.
//!
//! Sampling an allocation involves walking frames and interning strings, which
//! allocates through libc. If that path ever re-entered allocation tracking it
//! would corrupt the tracker's data structures, so a thread already inside the
//! hook declines to track.
//!
//! # Why this is allocation-free
//!
//! `thread_local!` with a `const` initialiser, over a type that needs no
//! `Drop`, lowers to a plain thread-local static: no lazy-initialisation
//! branch and no destructor registered with `__cxa_thread_atexit_impl` (glibc)
//! or `_tlv_atexit` (Darwin). `Cell<bool>` qualifies. Both properties are
//! asserted by `rust/tests/tls_alloc_free.rs`, which counts global allocator
//! traffic while hammering the guard.
//!
//! One honest caveat: on glibc and Darwin alike, the *first* access to a
//! thread-local in a `dlopen`ed library on a given thread can allocate, inside
//! the dynamic TLS machinery. The C++ had the same property. It is acceptable
//! because that allocation goes through libc `malloc`, never PyMem, so it
//! cannot re-enter our hooks. [`touch`] exists so start-up can take that hit
//! off the hook path.
//!
//! The C++ asked for `tls_model("global-dynamic")` explicitly, because an
//! `initial-exec` model would break when the extension is `dlopen`ed. Rust has
//! no stable attribute for this, but a `cdylib` is compiled PIC and LLVM
//! defaults thread-locals in PIC code to a dynamic model. `scripts/check_tls_model.sh`
//! checks the built object's relocations so a toolchain change cannot silently
//! regress it.

use std::cell::Cell;

thread_local! {
    /// True while this thread is inside the allocation-tracking path.
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// RAII guard. Acquires on construction if the current thread was not already
/// inside the hook, and releases on drop only if it did acquire.
///
/// Because the flag is thread-local, no atomics are involved.
#[must_use = "the guard releases on drop; check `acquired()` before tracking"]
pub struct Guard {
    acquired: bool,
}

impl Guard {
    /// Try to enter the tracking path on this thread.
    ///
    /// Callers must check [`Guard::acquired`] and skip tracking when it is
    /// false.
    pub fn acquire() -> Self {
        let acquired = IN_HOOK.with(|flag| {
            if flag.get() {
                false
            } else {
                flag.set(true);
                true
            }
        });
        Self { acquired }
    }

    /// Whether this guard, rather than an outer one, owns the flag.
    pub fn acquired(&self) -> bool {
        self.acquired
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Only release if we took it. A guard that failed to acquire does not
        // own the flag and must leave the outer guard's state alone.
        if self.acquired {
            IN_HOOK.with(|flag| flag.set(false));
        }
    }
}

/// Whether the current thread is inside the hook.
///
/// The `free` hook uses this for debug assertions only. It deliberately does
/// *not* take a [`Guard`]: skipping an untrack would leak tracker entries
/// forever, so freeing is always allowed to proceed.
/// (`cpp/_memalloc.cpp` only aborts on this in test builds.)
pub fn in_hook() -> bool {
    IN_HOOK.with(Cell::get)
}

/// Force the flag, used by the post-fork child handler to pin the surviving
/// thread as "inside a hook" while it tears down inherited state.
pub fn force_set(value: bool) {
    IN_HOOK.with(|flag| flag.set(value));
}

/// Clear the flag in a forked child.
///
/// Only the forking thread survives a fork, and it cannot have been inside the
/// hook (`os.fork` is not reachable from `PyObject_Malloc`), so this is
/// belt-and-braces rather than load-bearing. Other threads' thread-local
/// storage is simply unreachable inherited memory.
pub fn clear_for_fork_child() {
    force_set(false);
}

/// Touch the thread-local so the first-access allocation described in the
/// module docs happens here rather than inside an allocator hook.
pub fn touch() {
    let _ = in_hook();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]

    use super::{Guard, clear_for_fork_child, force_set, in_hook, touch};

    #[test]
    fn acquires_when_free_and_releases_on_drop() {
        assert!(!in_hook());
        {
            let g = Guard::acquire();
            assert!(g.acquired());
            assert!(in_hook());
        }
        assert!(!in_hook());
    }

    #[test]
    fn nested_acquire_is_declined_and_does_not_release_the_outer() {
        let outer = Guard::acquire();
        assert!(outer.acquired());
        {
            let inner = Guard::acquire();
            assert!(!inner.acquired(), "reentrant guard must not acquire");
            assert!(in_hook());
        }
        // Dropping the declined inner guard must leave the flag held.
        assert!(in_hook(), "inner guard released the outer guard's flag");
        drop(outer);
        assert!(!in_hook());
    }

    #[test]
    fn the_flag_is_per_thread() {
        let outer = Guard::acquire();
        assert!(in_hook());
        std::thread::spawn(|| {
            assert!(!in_hook(), "flag leaked across threads");
            let g = Guard::acquire();
            assert!(g.acquired());
        })
        .join()
        .unwrap();
        assert!(in_hook());
        drop(outer);
    }

    #[test]
    fn force_set_and_fork_child_clear() {
        force_set(true);
        assert!(in_hook());
        clear_for_fork_child();
        assert!(!in_hook());
    }

    #[test]
    fn touch_is_harmless() {
        touch();
        assert!(!in_hook());
    }

    /// The guard runs inside CPython's allocator, so acquiring it must not
    /// allocate. `thread_local!` with a `const` initialiser over a
    /// `!needs_drop` type is supposed to compile to a bare `#[thread_local]`
    /// static with no lazy-initialisation branch; this checks that empirically
    /// rather than trusting it, so a toolchain change cannot regress it
    /// silently.
    ///
    /// The first access on a thread may allocate inside the dynamic TLS
    /// machinery, which is why `touch()` is called outside the measurement --
    /// exactly as `start()` does in production.
    #[test]
    fn acquiring_the_guard_does_not_allocate() {
        std::thread::spawn(|| {
            // Warm the thread-local, and let the measurement harness warm its
            // own lazily-initialised state, outside the measured region.
            touch();
            let (_, warmup) = crate::memalloc::testing::count_allocations(in_hook);
            assert_eq!(warmup, 0, "reading the flag allocated {warmup} times");

            let (held, allocations) = crate::memalloc::testing::count_allocations(|| {
                let mut ever_held = false;
                for _ in 0..10_000 {
                    let g = Guard::acquire();
                    ever_held |= g.acquired();
                    assert!(in_hook());
                }
                ever_held
            });
            assert!(held, "guard never acquired");
            assert_eq!(
                allocations, 0,
                "10000 guard acquisitions allocated {allocations} times"
            );
        })
        .join()
        .unwrap();
    }

    /// Sanity check on the measurement itself: it must be able to see an
    /// allocation, otherwise the test above would pass vacuously.
    #[test]
    fn the_allocation_counter_actually_counts() {
        let (_, allocations) = crate::memalloc::testing::count_allocations(|| {
            let v: Vec<u8> = Vec::with_capacity(4096);
            std::hint::black_box(v.capacity())
        });
        assert!(allocations > 0, "counter observed nothing");
    }
}
