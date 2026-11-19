//! Up to 3.14 the thread state is a pthread key, and `pthread_getspecific` neither allocates
//! nor locks, so the API is async-signal-safe on every platform.

/// The calling thread's `PyThreadState`, or 0 if it has none. Async-signal-safe.
#[inline(always)]
pub fn current() -> usize {
    // SAFETY: this queries the calling thread's state without requiring the GIL.
    unsafe { pyo3_ffi::PyGILState_GetThisThreadState() as usize }
}
