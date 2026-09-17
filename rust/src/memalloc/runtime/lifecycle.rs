//! Starting and stopping the memory profiler.
//!
//! Replaces `memalloc_start` / `memalloc_stop` / `memalloc_heap_py` from
//! `cpp/_memalloc.cpp`. Argument validation produces the same exceptions with
//! the same messages the C++ raised, so nothing observable changes.

use crate::memalloc::limits::{MAX_HEAP_SAMPLE_SIZE, TRACEBACK_MAX_NFRAME};
use crate::memalloc::runtime::{heap, hooks, pyapi};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the hooks are currently installed.
///
/// Guarded by the GIL, which every caller holds, so a plain atomic is enough
/// to make the state visible rather than to serialise it.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Install the allocator hooks and create the heap tracker.
///
/// Must be called with the GIL held.
pub fn start(max_nframe: u16, heap_sample_size: u64, enable_mem_domain: bool) -> PyResult<()> {
    if ENABLED.load(Ordering::Acquire) {
        return Err(PyRuntimeError::new_err(
            "the memalloc module is already started",
        ));
    }

    if !(1..=TRACEBACK_MAX_NFRAME).contains(&max_nframe) {
        return Err(PyValueError::new_err(format!(
            "the number of frames must be in range [1; {TRACEBACK_MAX_NFRAME}]"
        )));
    }

    if heap_sample_size > MAX_HEAP_SAMPLE_SIZE {
        return Err(PyValueError::new_err(format!(
            "the heap sample size must be in range [0; {MAX_HEAP_SAMPLE_SIZE}]"
        )));
    }

    // Resolve and validate CPython's offsets *before* installing anything. If
    // this interpreter is not one we can read, decline rather than install
    // hooks that would produce garbage or crash.
    let resolved = pyapi::resolve();
    if let Err(error) = resolved {
        return Err(PyRuntimeError::new_err(pyapi::describe(&error)));
    }

    // PANIC-OK: bounded by the check above.
    #[allow(clippy::cast_possible_truncation)]
    let sample_size = heap_sample_size as u32;
    if !heap::heap_init(sample_size, max_nframe) {
        return Err(PyRuntimeError::new_err("failed to initialize heap tracker"));
    }

    // SAFETY: the caller holds the GIL and we have just checked that nothing
    // is installed.
    unsafe {
        hooks::install_hooks(enable_mem_domain);
    }
    ENABLED.store(true, Ordering::Release);
    Ok(())
}

/// Uninstall the hooks and destroy the heap tracker. Idempotent.
///
/// Must be called with the GIL held.
///
/// Order matters: the hooks come out first, so CPython stops dispatching to
/// us, and only then is the tracker torn down. The tracker must in turn be
/// torn down before the sink is reset, because its live samples hold interned
/// string IDs.
pub fn stop() {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    // SAFETY: the caller holds the GIL.
    unsafe {
        hooks::uninstall_hooks();
    }
    heap::heap_deinit();
    ENABLED.store(false, Ordering::Release);
}

/// Export every live sampled allocation into the profile. Idempotent.
///
/// Must be called with the GIL held.
pub fn flush() {
    if !ENABLED.load(Ordering::Acquire) {
        return;
    }
    heap::heap_flush();
}

/// Whether the profiler is currently running.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}
