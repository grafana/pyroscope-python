//! 3.15 keeps the thread state in a thread-local variable, and reading it through
//! `PyGILState_GetThisThreadState` is not known to be async-signal-safe on this platform.

use crate::ThreadStateError;

/// Always fails: no async-signal-safe way to find the thread state is implemented here.
///
/// # Safety
/// None.
pub unsafe fn init() -> Result<(), ThreadStateError> {
    Err(ThreadStateError::Unsupported)
}
