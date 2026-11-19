//! Reads a thread-local variable of another image with a `pthread_getspecific` call and a plain
//! memory load, without calling the variable's accessor. Everything here except
//! [`set_location`] is async-signal-safe.
//!
//! 3.15 keeps the thread state in a thread-local variable that is not exported. dyld gives
//! every image one pthread key, and a thread's value for that key is a block holding all of the
//! image's thread-local variables. The block is `malloc`ed on the thread's first access to any
//! of them and freed by the key's destructor at thread exit, so the accessor can allocate in a
//! thread that has not used the image's variables, or that is exiting. `ITIMER_PROF` delivers
//! the signal to any thread, including ones that never ran Python.
//!
//! `pthread_getspecific` is a load from the thread's slot array and never allocates, so
//! [`current`] reads the variable as the word at its offset in `pthread_getspecific(key)`, when
//! that is not null, at the [`Location`] stored with [`set_location`]. The module is compiled
//! for every CPython version, so that it and the search in `pyroscope_python_thread_state` are
//! tested on all of them, but the thread state is read this way only on 3.15.
//!
//! The key comes from the variable's descriptor, whose layout is private to dyld.
//! `pyroscope_python_thread_state` accepts a [`Location`] only after checking it on two threads
//! against dyld's own answer and the `malloc` block the key holds.
//!
//! Reads use kindasafe under the recovery preconditions documented at crate level.

use crate::{Error, read_usize};
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// One past the last pthread key, `_INTERNAL_POSIX_THREAD_KEYS_END` in libpthread.
/// `pthread_getspecific` does not check its argument.
pub const KEYS_END: u32 = 768;

/// Where a thread-local variable lives in every thread: at `offset` in the block held by the
/// pthread key `key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    pub key: u32,
    pub offset: usize,
}

/// The calling thread's block for `key`. [`Error::NoThreadState`] if it has not been
/// allocated, or if `key` is not below [`KEYS_END`]. Never allocates a block.
/// Async-signal-safe.
#[inline(always)]
pub fn block(key: u32) -> Result<usize, Error> {
    if key >= KEYS_END {
        return Err(Error::NoThreadState);
    }
    // SAFETY: `key` is below `KEYS_END`; the call loads the thread's slot for it.
    match unsafe { libc::pthread_getspecific(key as libc::pthread_key_t) } as usize {
        0 => Err(Error::NoThreadState),
        block => Ok(block),
    }
}

impl Location {
    /// The variable's value in the calling thread. [`Error::NoThreadState`] if the thread has
    /// no block for the image, which means it never accessed the image's variables or is
    /// exiting, [`Error::Read`] if the read failed. Async-signal-safe.
    #[inline(always)]
    pub fn read(self) -> Result<usize, Error> {
        read_usize(block(self.key)?, self.offset)
    }
}

/// No image has key 0.
const UNKNOWN: u32 = 0;
/// Never a valid key: not below [`KEYS_END`].
const WRITING: u32 = u32::MAX;

// Written once by `set_location`, `KEY` last.
static KEY: AtomicU32 = AtomicU32::new(UNKNOWN);
static OFFSET: AtomicUsize = AtomicUsize::new(0);

/// The location stored by [`set_location`], if any.
pub fn location() -> Option<Location> {
    match KEY.load(Ordering::Acquire) {
        UNKNOWN | WRITING => None,
        key => Some(Location {
            key,
            offset: OFFSET.load(Ordering::Relaxed),
        }),
    }
}

/// Makes [`current`] read the thread state at `location`. The first call wins: returns
/// `false` and changes nothing if a location was already set, or if `location.key` is 0 or
/// not below [`KEYS_END`]. Not async-signal-safe.
///
/// # Safety
/// `location` must hold CPython's thread state on every thread, verified as
/// `pyroscope_python_thread_state::init` does.
pub unsafe fn set_location(location: Location) -> bool {
    if location.key == UNKNOWN || location.key >= KEYS_END {
        return false;
    }
    if KEY
        .compare_exchange(UNKNOWN, WRITING, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    OFFSET.store(location.offset, Ordering::Relaxed);
    KEY.store(location.key, Ordering::Release);
    true
}

/// The calling thread's `PyThreadState`, or 0 if it has none or no location was set.
/// Async-signal-safe under kindasafe's recovery preconditions.
#[inline(always)]
pub fn current() -> usize {
    location()
        .and_then(|location| location.read().ok())
        .unwrap_or(0)
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn key_location_is_read_on_every_thread_and_set_once() {
        // SAFETY: tests preserve kindasafe's handlers and do not install other handlers.
        unsafe { kindasafe_init::init() }.unwrap();
        assert_eq!(location(), None);
        assert_eq!(current(), 0);

        let mut key: libc::pthread_key_t = 0;
        // SAFETY: creates a key without a destructor.
        assert_eq!(unsafe { libc::pthread_key_create(&mut key, None) }, 0);
        let key = key as u32;
        let invalid = Location {
            key: KEYS_END,
            offset: 0,
        };
        // SAFETY: rejected without being stored.
        assert!(!unsafe { set_location(invalid) });
        let at_key = Location {
            key,
            offset: size_of::<usize>(),
        };
        // SAFETY: every thread of this test stores a block with the value at `offset`.
        assert!(unsafe { set_location(at_key) });
        let other = Location { key, offset: 0 };
        // SAFETY: rejected without being stored.
        assert!(!unsafe { set_location(other) });
        assert_eq!(location(), Some(at_key));

        let set = move |value: usize| {
            let block = std::boxed::Box::leak(std::boxed::Box::new([0, value]));
            // SAFETY: `key` was created above; the block is leaked, so it stays valid.
            assert_eq!(
                unsafe { libc::pthread_setspecific(key as _, block.as_ptr().cast()) },
                0
            );
        };
        assert_eq!(current(), 0);
        set(0x2390);
        assert_eq!(current(), 0x2390);
        let on_thread = move |value| {
            thread::spawn(move || {
                if value != 0 {
                    set(value);
                }
                current()
            })
            .join()
            .unwrap()
        };
        assert_eq!(on_thread(0), 0);
        assert_eq!(on_thread(239), 239);
    }
}
