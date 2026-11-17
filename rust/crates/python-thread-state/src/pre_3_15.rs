use pyo3::Python;

pub(super) fn register_thread(_py: Python<'_>, _id: u64) -> Result<bool, libc::c_int> {
    Ok(true)
}

pub(super) fn unregister_thread(_py: Python<'_>, _id: u64) -> Result<(), libc::c_int> {
    Ok(())
}

pub(super) fn register_current_thread(_py: Python<'_>) -> Result<(), libc::c_int> {
    Ok(())
}

pub(super) fn unregister_current_thread(_py: Python<'_>) -> Result<(), libc::c_int> {
    Ok(())
}

#[inline(always)]
pub(super) fn current_thread_state() -> usize {
    // SAFETY: on 3.11–3.14 this queries CPython's pthread-specific state without the GIL.
    unsafe { pyo3::ffi::PyGILState_GetThisThreadState() as usize }
}
