//! Reads a thread-local variable of another module with plain memory loads, without entering
//! the dynamic loader. Everything here except [`set_location`] is async-signal-safe.
//!
//! 3.15 keeps the thread state in a thread-local variable that is not exported, and reaching
//! it through the API can allocate and lock inside glibc's dynamic loader. [`current`] reads it
//! at the [`Location`] stored with [`set_location`]. The module is compiled for every CPython
//! version, so that it and the search in `pyroscope_python_thread_state` are tested on all of
//! them, but the thread state is read this way only on 3.15.
//!
//! Compiled code reaches such a variable through `__tls_get_addr` or a TLS descriptor. For a
//! module in dynamic TLS, glibc allocates the thread's block on first access and may resize the
//! thread's DTV, so that path can call `malloc` and take loader locks.
//!
//! glibc keeps a module's block in one of two places:
//! - static TLS, at the same offset from the thread pointer in every thread: all modules
//!   loaded at startup, and `dlopen`ed ones that fit the static surplus;
//! - dynamic TLS, a separate allocation per thread, recorded in the thread's DTV.
//!
//! The DTV cannot serve both: a thread that existed before a module was placed in the static
//! surplus never gets a DTV entry for it, because static accesses do not go through the DTV.
//!
//! The TCB and DTV layouts are private to glibc. `pyroscope_python_thread_state` accepts a
//! [`Location`] only after checking it on two threads against the loader's own answer.
//!
//! Reads use kindasafe under the recovery preconditions documented at crate level.

use core::arch::asm;
use core::sync::atomic::{AtomicIsize, AtomicU8, AtomicUsize, Ordering};

/// `tcbhead_t::dtv`.
#[cfg(target_arch = "x86_64")]
const TCB_DTV: usize = 8;
#[cfg(target_arch = "aarch64")]
const TCB_DTV: usize = 0;

/// `sizeof(dtv_t)`. `dtv[-1]` holds the capacity and `dtv[module]` starts with the block address.
const DTV_ENTRY: usize = 16;

/// Where a thread-local variable lives in every thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// At `offset` from the thread pointer.
    Fixed { offset: isize },
    /// At `offset` in the block the thread's DTV records for `module`.
    Dtv { module: usize, offset: usize },
}

/// The calling thread's thread pointer. Async-signal-safe.
#[inline(always)]
pub fn thread_pointer() -> usize {
    let pointer: usize;
    // SAFETY: reads the thread pointer; x86-64 keeps a copy of it in the first TCB word.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!("mov {}, fs:[0]", out(reg) pointer, options(nostack, readonly, preserves_flags));
        #[cfg(target_arch = "aarch64")]
        asm!("mrs {}, tpidr_el0", out(reg) pointer, options(nomem, nostack, preserves_flags));
    }
    pointer
}

#[inline(always)]
fn read(address: usize) -> Option<usize> {
    // SAFETY: a pointer-sized load under kindasafe's recovery preconditions, documented at
    // crate level. No Rust reference to the source memory is created.
    unsafe { kindasafe::u64(address as u64) }
        .ok()
        .map(|word| word as usize)
}

/// The calling thread's block of `module` as its DTV records it, or `None` if the DTV has no
/// block for it. Never allocates one. Async-signal-safe.
#[inline(always)]
pub fn dtv_block(module: usize) -> Option<usize> {
    let dtv = read(thread_pointer().wrapping_add(TCB_DTV))?;
    let capacity = read(dtv.wrapping_sub(DTV_ENTRY))?;
    if module > capacity {
        return None;
    }
    // Never-used entries are zero, unallocated ones are TLS_DTV_UNALLOCATED.
    match read(dtv.wrapping_add(module.wrapping_mul(DTV_ENTRY)))? {
        0 | usize::MAX => None,
        block => Some(block),
    }
}

impl Location {
    /// The variable's value in the calling thread. `None` if the thread has no block for the
    /// module, which means it never accessed the variable, or if a read failed.
    /// Async-signal-safe.
    ///
    /// A DTV entry can outlive its module: glibc refreshes a thread's DTV only on that
    /// thread's next dynamic TLS access. If the module's id was reused from an unloaded
    /// module, a thread that has not accessed the variable can yield a word of the old block,
    /// so validate values read through [`Location::Dtv`].
    #[inline(always)]
    pub fn read(self) -> Option<usize> {
        match self {
            Location::Fixed { offset } => read(thread_pointer().wrapping_add_signed(offset)),
            Location::Dtv { module, offset } => read(dtv_block(module)?.wrapping_add(offset)),
        }
    }
}

const UNKNOWN: u8 = 0;
const FIXED: u8 = 1;
const DTV: u8 = 2;
const WRITING: u8 = 3;

// Written once by `set_location`, `KIND` last.
static KIND: AtomicU8 = AtomicU8::new(UNKNOWN);
static OFFSET: AtomicIsize = AtomicIsize::new(0);
static MODULE: AtomicUsize = AtomicUsize::new(0);

/// The location stored by [`set_location`], if any.
pub fn location() -> Option<Location> {
    let kind = KIND.load(Ordering::Acquire);
    let offset = OFFSET.load(Ordering::Relaxed);
    match kind {
        FIXED => Some(Location::Fixed { offset }),
        DTV => Some(Location::Dtv {
            module: MODULE.load(Ordering::Relaxed),
            offset: offset as usize,
        }),
        _ => None,
    }
}

/// Makes [`current`] read the thread state at `location`. The first call wins: returns
/// `false` and changes nothing if a location was already set. Not async-signal-safe.
///
/// # Safety
/// `location` must hold CPython's thread state on every thread, verified as
/// `pyroscope_python_thread_state::init` does.
pub unsafe fn set_location(location: Location) -> bool {
    if KIND
        .compare_exchange(UNKNOWN, WRITING, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    let kind = match location {
        Location::Fixed { offset } => {
            OFFSET.store(offset, Ordering::Relaxed);
            FIXED
        }
        Location::Dtv { module, offset } => {
            OFFSET.store(offset as isize, Ordering::Relaxed);
            MODULE.store(module, Ordering::Relaxed);
            DTV
        }
    };
    KIND.store(kind, Ordering::Release);
    true
}

/// The calling thread's `PyThreadState`, or 0 if it has none or no location was set.
/// Through the DTV, only a thread state whose `thread_id` is the calling thread is accepted,
/// so a thread running a state created on another thread has none. Async-signal-safe under
/// kindasafe's recovery preconditions.
#[inline(always)]
pub fn current() -> usize {
    match location() {
        Some(location @ Location::Fixed { .. }) => location.read().unwrap_or(0),
        // A stale DTV entry yields an arbitrary word: accept only a thread state bound to
        // this thread.
        Some(location @ Location::Dtv { .. }) => location
            .read()
            .filter(|&tstate| tstate != 0 && bound_to_this_thread(tstate))
            .unwrap_or(0),
        None => 0,
    }
}

#[inline(always)]
fn bound_to_this_thread(tstate: usize) -> bool {
    let thread_id = read(tstate.wrapping_add(crate::layout::THREAD_ID));
    // SAFETY: pthread_self only reads the thread pointer.
    thread_id == Some(unsafe { libc::pthread_self() } as usize)
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::thread;
    use std::thread_local;

    thread_local! {
        static TSTATE: Cell<usize> = const { Cell::new(0) };
    }

    #[test]
    fn fixed_location_is_read_on_every_thread_and_set_once() {
        // SAFETY: tests preserve kindasafe's handlers and do not install other handlers.
        unsafe { kindasafe_init::init() }.unwrap();
        assert_eq!(location(), None);
        assert_eq!(current(), 0);
        // The test binary's own TLS block is in static TLS.
        let offset = TSTATE.with(Cell::as_ptr) as isize - thread_pointer() as isize;
        let fixed = Location::Fixed { offset };
        // SAFETY: `TSTATE` is at `offset` on every thread of the test binary.
        assert!(unsafe { set_location(fixed) });
        let other = Location::Fixed { offset: 0 };
        // SAFETY: rejected without being stored.
        assert!(!unsafe { set_location(other) });
        assert_eq!(location(), Some(fixed));

        TSTATE.set(0x2390);
        assert_eq!(current(), 0x2390);
        let on_thread = |value| {
            thread::spawn(move || {
                if value != 0 {
                    TSTATE.set(value);
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
