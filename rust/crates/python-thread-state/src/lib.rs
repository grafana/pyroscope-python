#![no_std]

use pyo3::Python;

const LOG_TAG: &str = "Pyroscope::Signal";

#[cfg(Py_3_15)]
mod v3_15;
#[cfg(Py_3_15)]
use v3_15 as implementation;

#[cfg(not(Py_3_15))]
mod pre_3_15;
#[cfg(not(Py_3_15))]
use pre_3_15 as implementation;

// todo create a look up table for pre-configure existing threads on 3.15
pub fn register_thread(py: Python<'_>, id: u64) -> bool {
    match implementation::register_thread(py, id) {
        Ok(registered) => registered,
        Err(error) => {
            log::warn!(target: LOG_TAG, "failed to register Python thread state: pthread error {error}");
            false
        }
    }
}

pub fn unregister_thread(py: Python<'_>, id: u64) {
    if let Err(error) = implementation::unregister_thread(py, id) {
        log::warn!(target: LOG_TAG, "failed to clear Python thread state: pthread error {error}");
    }
}

pub fn register_current_thread(py: Python<'_>) -> Result<(), libc::c_int> {
    implementation::register_current_thread(py)
}

pub fn unregister_current_thread(py: Python<'_>) -> Result<(), libc::c_int> {
    implementation::unregister_current_thread(py)
}

pub struct ThreadState;

impl pyroscope_python_unwind::ThreadStateProvider for ThreadState {
    #[inline(always)]
    fn current_thread_state() -> usize {
        implementation::current_thread_state()
    }
}
