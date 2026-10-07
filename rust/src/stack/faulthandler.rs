//! Ported from dd-trace-py `ddtrace/profiling/_faulthandler.py`.
//!
//! Upstream hooks the import with `ModuleWatchdog`; `faulthandler` is not in
//! `sys.modules` at startup, so importing and patching it here is equivalent.

use pyo3::exceptions::PyImportError;
use pyo3::prelude::*;
use pyo3::types::PyModule;
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

const INSTALL_SRC: &std::ffi::CStr = cr#"
def install(faulthandler, threading, pause_sampling, resume_sampling, uninstall_segv_handler, reinstall_segv_handler):
    _original_enable = faulthandler.enable
    _original_disable = faulthandler.disable
    _enable_lock = threading.Lock()

    def _patched_enable(*args, **kwargs):
        with _enable_lock:
            # None means the sampler is running but did not pause in time:
            # swapping handlers now would race with safe_memcpy.
            pause_result = pause_sampling()
            safe_to_swap = pause_result is not None
            try:
                if safe_to_swap:
                    try:
                        uninstall_segv_handler()
                    except Exception:
                        pass

                    # Without a fresh install, faulthandler can save itself as its
                    # own previous handler and loop forever on a fault.
                    try:
                        _original_disable()
                    except Exception:
                        pass

                try:
                    _original_enable(*args, **kwargs)
                except Exception:
                    if safe_to_swap:
                        try:
                            reinstall_segv_handler()
                        except Exception:
                            pass
                    raise

                try:
                    reinstall_segv_handler()
                except Exception:
                    pass
            finally:
                if pause_result is True:
                    resume_sampling()

    def _patched_disable():
        with _enable_lock:
            pause_result = pause_sampling()
            safe_to_swap = pause_result is not None
            try:
                if not safe_to_swap:
                    return False

                try:
                    disabled = _original_disable()
                except Exception:
                    disabled = False

                # disable() restores the handler faulthandler saved, which need not be ours.
                try:
                    reinstall_segv_handler()
                except Exception:
                    pass

                return disabled
            finally:
                if pause_result is True:
                    resume_sampling()

    faulthandler.enable = _patched_enable
    faulthandler.disable = _patched_disable

    if faulthandler.is_enabled():
        try:
            reinstall_segv_handler()
        except Exception:
            pass
"#;

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

    let module = PyModule::from_code(
        py,
        INSTALL_SRC,
        c"pyroscope_stack_faulthandler.py",
        c"_pyroscope_stack_faulthandler",
    )?;
    module.getattr("install")?.call1((
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
