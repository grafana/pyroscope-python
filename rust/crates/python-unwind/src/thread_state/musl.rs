//! 3.15 keeps the thread state in a thread-local variable. musl allocates the TLS of a
//! `dlopen`ed module for all threads up front, so its TLS accesses are plain loads and the API
//! is async-signal-safe.

/// The calling thread's `PyThreadState`, or 0 if it has none. Async-signal-safe.
#[inline(always)]
pub fn current() -> usize {
    // SAFETY: this queries the calling thread's state without requiring the GIL.
    unsafe { pyo3_ffi::PyGILState_GetThisThreadState() as usize }
}
