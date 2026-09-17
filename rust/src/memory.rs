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
    unsafe {
        implementation::memalloc_heap_postfork_child();
    }
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
    use crate::encode::pprof::PProfBuilder;
    use crate::encode::pprof::StringTable;
    use crate::encode::pprof::ffi::{FFIFrame, FFISample};
    use crate::memalloc::pure::frames::{FrameSink, walk_frames};
    use crate::memalloc::runtime::pyapi;
    use crate::memalloc::runtime::reader::InProcess;
    use crate::utils::TimeRange;
    use lazy_static::lazy_static;
    use prost::Message;
    use pyo3::prelude::*;
    use std::ops::{Deref, DerefMut};
    use std::sync::Mutex;

    lazy_static! {
        static ref STRING_TABLE: Mutex<StringTable> = Mutex::new(StringTable::new());
    }

    lazy_static! {
        static ref PROFILE_BUILDER: Mutex<PProfBuilder> = Mutex::new(PProfBuilder::new());
    }
    unsafe extern "C" {
        pub fn memalloc_start(
            max_nframe: u16,
            heap_sample_size: u64,
            enable_mem_domain: bool,
        ) -> i32;
        pub fn memalloc_stop();
        // flush heap inuse samples
        pub fn memalloc_heap_py();
        pub fn memalloc_heap_postfork_child();
    }

    /// Writes collected frames into a caller-provided span, interning their
    /// strings.
    struct FfiFrameSink<'a> {
        out: &'a mut [FFIFrame],
        written: usize,
        strings: &'a mut StringTable,
    }

    impl FrameSink for FfiFrameSink<'_> {
        fn push_frame(&mut self, function: &str, file: &str, line: i32) {
            // PANIC-OK: `get_mut` returns None rather than panicking when the
            // span is full, which the frame cap should already have prevented.
            let Some(slot) = self.out.get_mut(self.written) else {
                return;
            };
            *slot = FFIFrame {
                function_name: (&self.strings.add(function)).into(),
                file_name: (&self.strings.add(file)).into(),
                line,
            };
            self.written = self.written.saturating_add(1);
        }

        fn note_dropped(&mut self) {
            // The C++ sample adapter has no field for this; the Rust-owned
            // sample in a later step will.
        }
    }

    /// Collect the current thread's Python stack into `out`.
    ///
    /// Returns the number of frames written, at most
    /// `min(max_nframe, out_cap)`, and 0 if the offsets table could not be
    /// validated.
    ///
    /// # Safety
    ///
    /// Called from inside the allocator hook, with the GIL held and the
    /// reentrancy guard already taken by the caller. `out` must point at
    /// `out_cap` writable `FFIFrame`s. Must not allocate through PyMem, touch
    /// refcounts, touch `PyErr`, or unwind.
    #[unsafe(no_mangle)]
    pub extern "C" fn pyroscope_memprof_collect_stack(
        max_nframe: u16,
        out: *mut FFIFrame,
        out_cap: usize,
    ) -> usize {
        if out.is_null() || out_cap == 0 {
            return 0;
        }
        let Ok(offsets) = pyapi::resolve() else {
            return 0;
        };

        let mut strings = STRING_TABLE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        // SAFETY: the caller guarantees `out` points at `out_cap` writable
        // `FFIFrame`s, and holds the GIL for the duration of this call so the
        // span cannot be reallocated underneath us.
        let span = unsafe { std::slice::from_raw_parts_mut(out, out_cap) };

        let budget = max_nframe.min(u16::try_from(out_cap).unwrap_or(u16::MAX));
        let mut sink = FfiFrameSink {
            out: span,
            written: 0,
            strings: &mut strings,
        };
        walk_frames(
            &InProcess,
            &offsets,
            &pyapi::type_addrs(),
            pyapi::current_thread_state(),
            budget,
            &mut sink,
        );
        sink.written
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn pyroscope_memprof_push_sample(sample: FFISample) {
        if sample.frames.is_null() || sample.len == 0 {
            return;
        }
        let frames = unsafe { std::slice::from_raw_parts(sample.frames, sample.len) };
        if let Ok(mut pb) = PROFILE_BUILDER.lock() {
            pb.add_ffi_sample(frames, &sample.values);
        }
    }

    /// Discard all interned strings and buffered samples.
    ///
    /// Called from `stop()` after the allocator hooks are uninstalled. Every
    /// hook runs with the GIL held and `stop()` itself holds the GIL, so no
    /// hook can be mid-push here and no live C++ traceback references the
    /// interned string IDs anymore. Without this, samples buffered by a
    /// stopped session (or inherited from the parent after fork, since the
    /// fork-child handler also goes through `stop()`) would leak into the
    /// next session's first profile, and the string table would grow for the
    /// lifetime of the process.
    pub fn clear_state() {
        let mut st = STRING_TABLE.lock().unwrap_or_else(|e| e.into_inner());
        *st = StringTable::new();
        drop(st);
        let mut pb = PROFILE_BUILDER.lock().unwrap_or_else(|e| e.into_inner());
        pb.reset();
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
            unsafe {
                memalloc_heap_py();
            }
            let st = STRING_TABLE.lock();
            let pb = PROFILE_BUILDER.lock();
            match (st, pb) {
                (Ok(mut st), Ok(mut pb)) => {
                    pb.set_memory_profile_type(st.deref_mut(), heap_sample_size);
                    pb.take_profile_and_reset(st.deref(), time_range)
                }
                _ => None,
            }
        })??;
        Some(profile.encode_to_vec())
    }
}

#[cfg(not(feature = "memory"))]
mod implementation {
    use crate::utils::TimeRange;

    pub unsafe fn memalloc_stop() {}

    pub unsafe fn memalloc_heap_postfork_child() {}

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
