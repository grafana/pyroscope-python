#![no_std]

//! Finds the calling thread's `PyThreadState` from a signal handler, like
//! `PyGILState_GetThisThreadState` but without the dynamic loader's TLS access path.
//!
//! Call [`init`] once on a normal thread, then [`current`] from the handler.
//!
//! CPython 3.15 keeps the thread state in a thread-local variable, and reaching it through
//! `PyGILState_GetThisThreadState` can allocate and lock inside glibc's dynamic loader. On
//! glibc (x86-64 and aarch64) [`current`] reads the variable directly: at a fixed offset from
//! the thread pointer, or through the thread's DTV when libpython was `dlopen`ed into dynamic
//! TLS. [`init`] picks one and verifies it against the loader on two threads, because both
//! rely on glibc's private TCB and DTV layouts. Through the DTV, only a thread state whose
//! `thread_id` is the calling thread is accepted, so a thread running a state created on
//! another thread has none. musl needs none of this: its TLS accesses are plain loads. Other
//! platforms are unsupported on 3.15.
//!
//! Direct reads use kindasafe: recovery must be installed and its signals unblocked on the
//! calling thread, as for the unwinder.

#[cfg(test)]
extern crate std;

#[cfg(all(
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[cfg_attr(not(Py_3_15), allow(dead_code))]
mod tls;

/// How the signal handler finds the thread state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStateAccess {
    /// `PyGILState_GetThisThreadState`.
    Api,
    /// CPython's thread-local variable, at a fixed offset from the thread pointer.
    FixedOffset,
    /// CPython's thread-local variable, in the block recorded in the thread's DTV.
    Dtv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStateError {
    /// No async-signal-safe way to find the thread state is implemented for this platform.
    Unsupported,
    /// The module defining `PyGILState_GetThisThreadState` has no TLS segment.
    NoTlsSegment,
    /// The TLS block of CPython does not hold the thread state in exactly one slot.
    VariableNotFound,
    /// The variable's location did not hold on two threads.
    UnknownTlsLayout,
}

pub use imp::{current, init};

// Up to 3.14 the thread state is a pthread key, and `pthread_getspecific` neither allocates
// nor locks. musl allocates the TLS of a `dlopen`ed module for all threads up front, so its
// TLS accesses are plain loads.
#[cfg(any(not(Py_3_15), all(target_os = "linux", target_env = "musl")))]
mod imp {
    use super::*;

    /// Prepares [`current`] to find the thread state.
    ///
    /// # Safety
    /// The interpreter must be initialized.
    pub unsafe fn init() -> Result<ThreadStateAccess, ThreadStateError> {
        Ok(ThreadStateAccess::Api)
    }

    /// The calling thread's `PyThreadState`, or 0 if it has none. Async-signal-safe.
    #[inline(always)]
    pub fn current() -> usize {
        // SAFETY: this queries the calling thread's state without requiring the GIL.
        unsafe { pyo3_ffi::PyGILState_GetThisThreadState() as usize }
    }
}

// 3.15 keeps the thread state in a thread-local variable that is not exported.
#[cfg(all(
    Py_3_15,
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod imp {
    use super::*;
    use crate::tls::{self, Location};
    use core::sync::atomic::{AtomicIsize, AtomicU8, AtomicUsize, Ordering};
    use pyroscope_python_offsets as layout;

    const UNKNOWN: u8 = 0;
    const FIXED: u8 = 1;
    const DTV: u8 = 2;

    // Written once, `KIND` last.
    static KIND: AtomicU8 = AtomicU8::new(UNKNOWN);
    static OFFSET: AtomicIsize = AtomicIsize::new(0);
    static MODULE: AtomicUsize = AtomicUsize::new(0);

    fn access(kind: u8) -> Option<ThreadStateAccess> {
        match kind {
            FIXED => Some(ThreadStateAccess::FixedOffset),
            DTV => Some(ThreadStateAccess::Dtv),
            _ => None,
        }
    }

    /// Locates CPython's thread-local thread state, so that [`current`] reads it without
    /// calling into CPython or the dynamic loader. Until this succeeds [`current`] finds no
    /// thread state. The location is cached for the process.
    ///
    /// Briefly releases the GIL and spawns a short-lived thread.
    ///
    /// # Safety
    /// The interpreter must be initialized and kindasafe recovery must be installed.
    pub unsafe fn init() -> Result<ThreadStateAccess, ThreadStateError> {
        if let Some(access) = access(KIND.load(Ordering::Acquire)) {
            return Ok(access);
        }
        let module = tls::module_of(pyo3_ffi::PyGILState_GetThisThreadState as *const () as usize)
            .ok_or(ThreadStateError::NoTlsSegment)?;
        // SAFETY: the interpreter is initialized. The thread is detached during the search
        // because the attached thread state is also cached in a second thread-local variable.
        let offset = unsafe {
            let gil = pyo3_ffi::PyGILState_Ensure();
            let tstate = pyo3_ffi::PyGILState_GetThisThreadState() as usize;
            let saved = pyo3_ffi::PyEval_SaveThread();
            let offset = tls::find_value(module, tstate);
            pyo3_ffi::PyEval_RestoreThread(saved);
            pyo3_ffi::PyGILState_Release(gil);
            offset
        }
        .ok_or(ThreadStateError::VariableNotFound)?;
        let kind = match tls::locate(module, offset).ok_or(ThreadStateError::UnknownTlsLayout)? {
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
        Ok(access(kind).unwrap())
    }

    /// The calling thread's `PyThreadState`, or 0 if it has none or [`init`] has not
    /// succeeded. Async-signal-safe under kindasafe's recovery preconditions.
    #[inline(always)]
    pub fn current() -> usize {
        let kind = KIND.load(Ordering::Acquire);
        let offset = OFFSET.load(Ordering::Relaxed);
        match kind {
            FIXED => Location::Fixed { offset }.read().unwrap_or(0),
            DTV => {
                let location = Location::Dtv {
                    module: MODULE.load(Ordering::Relaxed),
                    offset: offset as usize,
                };
                // A stale DTV entry yields an arbitrary word: accept only a thread state
                // bound to this thread.
                location
                    .read()
                    .filter(|&tstate| tstate != 0 && bound_to_this_thread(tstate))
                    .unwrap_or(0)
            }
            _ => 0,
        }
    }

    #[inline(always)]
    fn bound_to_this_thread(tstate: usize) -> bool {
        // SAFETY: a pointer-sized load under kindasafe's recovery preconditions.
        let thread_id = unsafe { kindasafe::u64(tstate.wrapping_add(layout::THREAD_ID) as u64) };
        // SAFETY: pthread_self only reads the thread pointer.
        thread_id == Ok(unsafe { libc::pthread_self() } as u64)
    }
}

#[cfg(not(any(
    not(Py_3_15),
    all(target_os = "linux", target_env = "musl"),
    all(
        target_os = "linux",
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )
)))]
mod imp {
    use super::*;

    /// Always fails: `PyGILState_GetThisThreadState` reads a thread-local variable, which is
    /// not known to be async-signal-safe on this platform.
    ///
    /// # Safety
    /// None.
    pub unsafe fn init() -> Result<ThreadStateAccess, ThreadStateError> {
        Err(ThreadStateError::Unsupported)
    }

    /// Always 0: no thread state can be found on this platform.
    #[inline(always)]
    pub fn current() -> usize {
        0
    }
}
