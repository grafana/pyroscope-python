//! musl's TLS accesses are plain loads, so on 3.15 the handler calls
//! `PyGILState_GetThisThreadState`, which is async-signal-safe there.

use crate::ThreadStateError;
use pyo3::Python;

/// Nothing to prepare: the handler calls `PyGILState_GetThisThreadState`.
///
/// # Safety
/// None. Unsafe only to match the glibc variant, which needs kindasafe recovery.
pub unsafe fn init(_py: Python<'_>) -> Result<(), ThreadStateError> {
    Ok(())
}
