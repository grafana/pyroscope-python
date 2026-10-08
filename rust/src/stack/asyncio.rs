//! Ported from dd-trace-py `ddtrace/profiling/_asyncio.py`.
//!
//! Upstream hooks the `asyncio` and `uvloop` imports with `ModuleWatchdog`; we
//! have no import-hook machinery, so `install` patches whichever of the two is
//! already in `sys.modules` and skips the rest. See `stack_known_bugs.md`.

use pyo3::ffi::PyObject;
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;

unsafe extern "C" {
    fn pyroscope_stack_init_asyncio(scheduled_tasks: *mut PyObject, eager_tasks: *mut PyObject);
    fn pyroscope_stack_track_asyncio_loop(thread_id: u64, event_loop: *mut PyObject);
    fn pyroscope_stack_link_tasks(parent: *mut PyObject, child: *mut PyObject);
    fn pyroscope_stack_weak_link_tasks(parent: *mut PyObject, child: *mut PyObject);
    fn pyroscope_stack_set_uvloop_mode(thread_id: u64, value: bool) -> bool;
}

static INSTALLED: OnceLock<()> = OnceLock::new();

/// Not under the GIL: this takes echion's `thread_info_map_lock`, which the
/// sampling thread holds while interning strings.
#[pyfunction]
fn track_asyncio_loop(py: Python<'_>, thread_id: u64, event_loop: &Bound<'_, PyAny>) {
    let event_loop = event_loop.as_ptr() as usize;
    py.detach(|| unsafe {
        pyroscope_stack_track_asyncio_loop(thread_id, event_loop as *mut PyObject)
    });
}

#[pyfunction]
fn init_asyncio(scheduled_tasks: &Bound<'_, PyAny>, eager_tasks: &Bound<'_, PyAny>) {
    unsafe { pyroscope_stack_init_asyncio(scheduled_tasks.as_ptr(), eager_tasks.as_ptr()) }
}

/// Pyroscope patch: upstream keeps filling the link maps after `stop()`,
/// where nothing prunes them and every `create_task` adds an entry.
#[pyfunction]
fn link_tasks(py: Python<'_>, parent: &Bound<'_, PyAny>, child: &Bound<'_, PyAny>) {
    if !super::STARTED.load(Ordering::Acquire) {
        return;
    }
    let (parent, child) = (parent.as_ptr() as usize, child.as_ptr() as usize);
    py.detach(|| unsafe {
        pyroscope_stack_link_tasks(parent as *mut PyObject, child as *mut PyObject)
    });
}

#[pyfunction]
fn weak_link_tasks(py: Python<'_>, parent: &Bound<'_, PyAny>, child: &Bound<'_, PyAny>) {
    if !super::STARTED.load(Ordering::Acquire) {
        return;
    }
    let (parent, child) = (parent.as_ptr() as usize, child.as_ptr() as usize);
    py.detach(|| unsafe {
        pyroscope_stack_weak_link_tasks(parent as *mut PyObject, child as *mut PyObject)
    });
}

/// Pyroscope patch: upstream keeps the GIL here, unlike its own
/// `track_asyncio_loop`, though both take `thread_info_map_lock`.
#[pyfunction]
fn set_uvloop_mode(py: Python<'_>, thread_id: u64, value: bool) {
    let applied = py.detach(|| unsafe { pyroscope_stack_set_uvloop_mode(thread_id, value) });
    let state = if value { "enabled" } else { "disabled" };
    if applied {
        log::info!(
            target: "pyroscope-python",
            "uvloop task unwinding {state} for thread {thread_id}"
        );
    } else {
        log::warn!(
            target: "pyroscope-python",
            "uvloop task unwinding not {state} for thread {thread_id}: the thread is not \
             registered"
        );
    }
}

/// Must run after `threads::install`: `track_asyncio_loop` is a
/// find-then-mutate on echion's thread map and is dropped for a thread that
/// is not registered yet.
pub fn install(py: Python<'_>) {
    if INSTALLED.set(()).is_err() {
        return;
    }

    let patched = (|| -> PyResult<()> {
        let modules = py.import("sys")?.getattr("modules")?;
        let Ok(asyncio) = modules.get_item("asyncio") else {
            log::warn!(
                target: "pyroscope-python",
                "not tracking asyncio tasks: asyncio is not imported yet, and cpu_async only \
                 patches what is imported when the agent starts"
            );
            return Ok(());
        };

        let uvloop = modules.get_item("uvloop").ok();
        if uvloop.is_none() {
            log::warn!(
                target: "pyroscope-python",
                "not tracking uvloop event loops: uvloop is not imported yet, and cpu_async \
                 only patches what is imported when the agent starts; harmless if this \
                 process does not use uvloop, otherwise import uvloop before configure()"
            );
        }

        py.import("pyroscope")?
            .getattr("_install_stack_asyncio")?
            .call1((
                asyncio,
                py.import("threading")?,
                uvloop,
                wrap_pyfunction!(track_asyncio_loop, py)?,
                wrap_pyfunction!(init_asyncio, py)?,
                wrap_pyfunction!(link_tasks, py)?,
                wrap_pyfunction!(weak_link_tasks, py)?,
                wrap_pyfunction!(set_uvloop_mode, py)?,
            ))?;
        Ok(())
    })();

    if let Err(err) = patched {
        log::warn!(target: "pyroscope-python", "not tracking asyncio tasks: {err}");
    }
}
