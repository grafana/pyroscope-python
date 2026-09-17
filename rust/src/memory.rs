use crate::utils::TimeRange;
#[cfg(feature = "memory")]
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
    pub enable_mem_domain: bool,
    pub max_nframe: u16,
    pub heap_sample_size: u64,
}

pub fn start(py: Python<'_>, config: &Config) -> PyResult<()> {
    if !config.enabled {
        return Ok(());
    }

    #[cfg(not(feature = "memory"))]
    {
        let _ = py;
        // Built without the `memory` feature. setup.py enables it only on
        // CPython 3.13+ with the GIL enabled; see MEMORY_MIN_PYTHON there for
        // why. Accept and ignore rather than failing, so a caller passing
        // mem_enabled still gets CPU profiling.
        log::warn!(
            target: "pyroscope-python",
            "Memory profiling was enabled, but this build does not include memory profiling support \
             (it requires CPython 3.13+ with the GIL enabled); mem_enabled will be ignored."
        );
        Ok(())
    }

    #[cfg(feature = "memory")]
    unsafe {
        if let Some(err) = PyErr::take(py) {
            return Err(err);
        }

        let status = implementation::memalloc_start(
            config.max_nframe,
            config.heap_sample_size,
            config.enable_mem_domain,
        );
        let err = PyErr::take(py);
        match (status, err) {
            (0, None) => Ok(()),
            (0, Some(err)) => {
                implementation::memalloc_stop();
                implementation::clear_state();
                Err(err)
            }
            (_, Some(err)) => Err(err),
            (_, None) => Err(PyRuntimeError::new_err(
                "memory profiler failed to start without setting a Python exception",
            )),
        }
    }
}

pub fn stop(_py: Python<'_>) {
    unsafe {
        implementation::memalloc_stop();
    }
    implementation::clear_state();
}

pub fn postfork_child() {
    implementation::postfork_child();
}

pub fn dump_pprof(heap_sample_size: u64, time_range: &TimeRange) -> Option<Vec<u8>> {
    implementation::dump_pprof(heap_sample_size, time_range)
}

/// What [`offsets_report`] returns: the CPython version this extension was
/// built for, an error string if the offsets table could not be validated,
/// and the resolved field offsets.
pub type OffsetsReport = ((u8, u8), Option<String>, Vec<(&'static str, usize)>);

/// What [`walk_stack`] returns: `(function, file, line)` per frame,
/// innermost first.
pub type WalkedStack = Vec<(String, String, i32)>;

/// Walk the calling thread's Python stack with the profiler's own walker.
///
/// Diagnostic only. `scripts/check_frame_walk.py` compares the result against
/// `traceback.extract_stack()`, which is the check that a new CPython version
/// is actually being read correctly.
pub fn walk_stack(max_nframe: u16) -> Result<WalkedStack, String> {
    implementation::walk_stack(max_nframe)
}

/// Diagnostic report on this interpreter's `_Py_DebugOffsets`.
///
/// Returns the CPython version this extension was built for, an error string
/// if the table could not be validated, and the resolved field offsets.
/// `scripts/check_debug_offsets.py` compares these against ctypes reads of the
/// same interpreter, which is what catches a transcription error in a mirror
/// before it can corrupt a profile.
pub fn offsets_report() -> OffsetsReport {
    implementation::offsets_report()
}

#[cfg(feature = "memory")]
mod implementation {
    use crate::memalloc::pure::frames::walk_frames;
    use crate::memalloc::runtime::reader::InProcess;
    use crate::memalloc::runtime::{heap, pyapi};
    use crate::memalloc::sink::{self, MemSink};
    use crate::utils::TimeRange;
    use prost::Message;
    use pyo3::prelude::*;

    unsafe extern "C" {
        pub fn memalloc_start(
            max_nframe: u16,
            heap_sample_size: u64,
            enable_mem_domain: bool,
        ) -> i32;
        pub fn memalloc_stop();
    }

    /// Discard all interned strings and buffered samples.
    ///
    /// Called from `stop()` after the allocator hooks are uninstalled. Every
    /// hook runs with the GIL held and `stop()` itself holds the GIL, so no
    /// hook can be mid-push here.
    ///
    /// Must run *after* the heap tracker is torn down: its live samples hold
    /// interned string IDs, which this invalidates. Without it, samples
    /// buffered by a stopped session (or inherited from the parent after a
    /// fork, since that handler also goes through `stop()`) would leak into
    /// the next session's first profile, and the string table would grow for
    /// the lifetime of the process.
    pub fn clear_state() {
        sink::lock().reset();
    }

    pub fn postfork_child() {
        heap::pyroscope_memprof_heap_postfork_child();
    }

    pub fn walk_stack(max_nframe: u16) -> Result<super::WalkedStack, String> {
        use crate::memalloc::pure::frames::CollectedFrames;

        let offsets = pyapi::resolve().map_err(|error| pyapi::describe(&error))?;
        let mut collected = CollectedFrames::default();
        walk_frames(
            &InProcess,
            &offsets,
            &pyapi::type_addrs(),
            pyapi::current_thread_state(),
            max_nframe,
            &mut collected,
        );
        Ok(collected.frames)
    }

    pub fn offsets_report() -> super::OffsetsReport {
        use crate::memalloc::pure::offsets;

        let build = (offsets::build_major(), offsets::build_minor());
        match pyapi::resolve() {
            Ok(resolved) => (build, None, pyapi::report(&resolved)),
            Err(error) => (build, Some(pyapi::describe(&error)), Vec::new()),
        }
    }

    pub fn dump_pprof(heap_sample_size: u64, time_range: &TimeRange) -> Option<Vec<u8>> {
        // try_attach skips the flush while finalizing on 3.13+ only; on
        // older Pythons the atexit hook is the actual protection.
        let profile = Python::try_attach(|_| {
            heap::pyroscope_memprof_heap_flush();
            let mut guard = sink::lock();
            let MemSink { strings, builder } = &mut *guard;
            builder.set_memory_profile_type(strings, heap_sample_size);
            builder.take_profile_and_reset(strings, time_range)
        })??;
        // Deliberately outside the lock: serialising a multi-megabyte profile
        // while holding it would block every allocator hook in the process.
        Some(profile.encode_to_vec())
    }
}

#[cfg(not(feature = "memory"))]
mod implementation {
    use crate::utils::TimeRange;

    pub unsafe fn memalloc_stop() {}

    pub fn postfork_child() {}

    pub fn clear_state() {}

    pub fn dump_pprof(_heap_sample_size: u64, _time_range: &TimeRange) -> Option<Vec<u8>> {
        None
    }

    pub fn walk_stack(_max_nframe: u16) -> Result<super::WalkedStack, String> {
        Err("this build does not include memory profiling support".to_owned())
    }

    pub fn offsets_report() -> super::OffsetsReport {
        // The build-time version is recorded by build.rs in every feature
        // configuration, so report it even here: it is what tells a user
        // whether they are on an unsupported interpreter or simply have a
        // wheel built without memory support.
        use crate::memalloc::pure::offsets;
        (
            (offsets::build_major(), offsets::build_minor()),
            Some(
                concat!(
                    "this build does not include memory profiling support ",
                    "(it requires CPython 3.13+ with the GIL enabled)"
                )
                .to_owned(),
            ),
            Vec::new(),
        )
    }
}
