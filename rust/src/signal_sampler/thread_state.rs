//! CPython 3.15 thread-state cache for signal-handler reads.
//!
//! Registration may allocate and must run on the thread being registered, outside a
//! signal handler. The key lives for the process lifetime, including profiler restarts.
//! We rely on the supported libcs' pthread_getspecific implementation being a direct
//! TLS lookup without locks or allocation; POSIX does not guarantee signal safety.

use pyo3::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const UNINITIALIZED: usize = usize::MAX;
static KEY: AtomicUsize = AtomicUsize::new(UNINITIALIZED);

fn get_or_create_key(_py: Python<'_>) -> Result<libc::pthread_key_t, libc::c_int> {
    let mut key = KEY.load(Ordering::Acquire);
    if key == UNINITIALIZED {
        let mut candidate = 0;
        let error = unsafe { libc::pthread_key_create(&mut candidate, None) };
        if error != 0 {
            return Err(error);
        }
        key = match KEY.compare_exchange(
            UNINITIALIZED,
            candidate as usize,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => candidate as usize,
            Err(existing) => {
                // This candidate was never published or used for thread data.
                unsafe { libc::pthread_key_delete(candidate) };
                existing
            }
        };
    }
    Ok(key as libc::pthread_key_t)
}

/// Cache the attached state outside the signal handler. The thread exit hook clears
/// it before Python tears down the state. Hooks and the key survive profiler restarts.
/// Switching interpreters or replacing the state on a registered thread is unsupported.
pub(crate) fn register_current(_py: Python<'_>) -> Result<(), libc::c_int> {
    let error = unsafe {
        libc::pthread_setspecific(
            get_or_create_key(_py)?,
            pyo3::ffi::PyThreadState_Get().cast(),
        )
    };
    if error == 0 { Ok(()) } else { Err(error) }
}

/// Clear the calling thread's cache before Python destroys its state.
/// Call outside a signal handler.
pub(crate) fn unregister_current(_py: Python<'_>) -> Result<(), libc::c_int> {
    let key = KEY.load(Ordering::Acquire);
    if key == UNINITIALIZED {
        return Ok(());
    }
    let error = unsafe { libc::pthread_setspecific(key as libc::pthread_key_t, core::ptr::null()) };
    if error == 0 { Ok(()) } else { Err(error) }
}

pub(super) fn current() -> usize {
    let key = KEY.load(Ordering::Acquire);
    if key == UNINITIALIZED {
        return 0;
    }
    // The published key is never deleted. This path never initializes TLS or calls Python.
    unsafe { libc::pthread_getspecific(key as libc::pthread_key_t) as usize }
}
