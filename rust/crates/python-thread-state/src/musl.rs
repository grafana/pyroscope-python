//! musl's TLS accesses are plain loads, so on 3.15 the handler calls
//! `PyGILState_GetThisThreadState`, which is async-signal-safe there.

use crate::{ThreadStateAccess, ThreadStateError};

/// Nothing to prepare: the handler calls `PyGILState_GetThisThreadState`.
///
/// # Safety
/// The interpreter must be initialized.
pub unsafe fn init() -> Result<ThreadStateAccess, ThreadStateError> {
    Ok(ThreadStateAccess::Api)
}
