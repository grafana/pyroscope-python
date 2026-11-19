//! 3.15 keeps the thread state in a thread-local variable, and reading it through
//! `PyGILState_GetThisThreadState` is not known to be async-signal-safe on this platform.

use crate::ThreadStateError;
use pyo3::Python;

/// Always fails: no async-signal-safe way to find the thread state is implemented here.
///
/// # Safety
/// None. Unsafe only to match the glibc variant, which needs kindasafe recovery.
pub unsafe fn init(_py: Python<'_>) -> Result<(), ThreadStateError> {
    Err(ThreadStateError::Unsupported)
}
