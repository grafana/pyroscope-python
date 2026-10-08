//! Ported from dd-trace-py `ddtrace/profiling/collector/threading.py::init_stack`.

use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use std::sync::OnceLock;

unsafe extern "C" {
    fn pyroscope_stack_register_thread(id: u64, native_id: u64);
    fn pyroscope_stack_unregister_thread(id: u64);
}

static INSTALLED: OnceLock<()> = OnceLock::new();

#[pyfunction]
fn register_thread(py: Python<'_>, id: u64, native_id: u64) {
    log::debug!(
        target: "pyroscope-python",
        "registering thread id={id} native_id={native_id}"
    );
    py.detach(|| unsafe { pyroscope_stack_register_thread(id, native_id) });
}

#[pyfunction]
fn unregister_thread(py: Python<'_>, id: u64) {
    log::debug!(target: "pyroscope-python", "unregistering thread id={id}");
    py.detach(|| unsafe { pyroscope_stack_unregister_thread(id) });
}

/// The wrappers must be Python-level functions: a `#[pyfunction]` is a
/// `builtin_function_or_method` and has no `__get__`, so assigning one onto
/// `Thread._set_native_id` would call it without `self`.
pub fn install(py: Python<'_>) -> PyResult<()> {
    if INSTALLED.get().is_some() {
        return Ok(());
    }

    py.import("pyroscope")?
        .getattr("_install_stack_threads")?
        .call1((
            py.import("threading")?,
            wrap_pyfunction!(register_thread, py)?,
            wrap_pyfunction!(unregister_thread, py)?,
        ))?;

    let _ = INSTALLED.set(());
    Ok(())
}
