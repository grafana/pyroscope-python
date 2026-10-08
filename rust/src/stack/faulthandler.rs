//! Ported from dd-trace-py `ddtrace/profiling/_faulthandler.py`.
//!
//! Upstream hooks the import with `ModuleWatchdog`; `faulthandler` is not in
//! `sys.modules` at startup, so importing and patching it here is equivalent.

use pyo3::exceptions::PyImportError;
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use std::sync::OnceLock;

// Keep in sync with Datadog::PauseResult in dd-trace-py's stack/include/sampler.hpp.
#[allow(dead_code)]
#[repr(u8)]
enum PauseResult {
    Paused = 0,
    NotRunning = 1,
    Timeout = 2,
}

unsafe extern "C" {
    fn pyroscope_stack_pause_sampling() -> PauseResult;
    fn pyroscope_stack_resume_sampling();
    fn pyroscope_stack_uninstall_segv_handler();
    fn pyroscope_stack_reinstall_segv_handler();
}

static INSTALLED: OnceLock<()> = OnceLock::new();

/// Not under the GIL: `Sampler::pause` waits up to 3 s for the sampling
/// thread, which interns strings.
#[pyfunction]
fn pause_sampling(py: Python<'_>) -> Option<bool> {
    match py.detach(|| unsafe { pyroscope_stack_pause_sampling() }) {
        PauseResult::Paused => Some(true),
        PauseResult::NotRunning => Some(false),
        PauseResult::Timeout => None,
    }
}

#[pyfunction]
fn resume_sampling() {
    unsafe { pyroscope_stack_resume_sampling() }
}

#[pyfunction]
fn uninstall_segv_handler() {
    unsafe { pyroscope_stack_uninstall_segv_handler() }
}

#[pyfunction]
fn reinstall_segv_handler() {
    unsafe { pyroscope_stack_reinstall_segv_handler() }
}

pub fn install(py: Python<'_>) -> PyResult<()> {
    if INSTALLED.get().is_some() {
        return Ok(());
    }

    let faulthandler = match py.import("faulthandler") {
        Ok(module) => module,
        Err(err) if err.is_instance_of::<PyImportError>(py) => {
            log::warn!(
                target: "pyroscope-python",
                "not patching faulthandler: {err}; a later faulthandler.enable() \
                 can displace the fast copy's SIGSEGV handler"
            );
            let _ = INSTALLED.set(());
            return Ok(());
        }
        Err(err) => return Err(err),
    };

    py.import("pyroscope")?
        .getattr("_install_stack_faulthandler")?
        .call1((
            faulthandler,
            py.import("threading")?,
            wrap_pyfunction!(pause_sampling, py)?,
            wrap_pyfunction!(resume_sampling, py)?,
            wrap_pyfunction!(uninstall_segv_handler, py)?,
            wrap_pyfunction!(reinstall_segv_handler, py)?,
        ))?;

    let _ = INSTALLED.set(());
    Ok(())
}
