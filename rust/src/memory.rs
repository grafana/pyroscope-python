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
        log::warn!(
            target: "pyroscope-python",
            "Memory profiling was enabled, but this build does not include memory profiling support; mem_enabled will be ignored."
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

#[cfg(feature = "memory")]
mod implementation {
    use crate::encode::pprof::PProfBuilder;
    use crate::encode::pprof::ffi::{FFIInternedString, FFISample, FFIStringView};
    use crate::encode::pprof::{StringID, StringTable};
    use crate::utils::TimeRange;
    use lazy_static::lazy_static;
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

    #[unsafe(no_mangle)]
    pub extern "C" fn pyroscope_memprof_string_table_intern_string(
        s: FFIStringView,
    ) -> FFIInternedString {
        if s.data.is_null() || s.len == 0 {
            return StringID::empty_ffi_string();
        }
        let unsafe_str = unsafe {
            let s = std::slice::from_raw_parts(s.data as *const u8, s.len);
            std::str::from_utf8_unchecked(s)
        };
        match STRING_TABLE.lock() {
            Ok(mut string_table) => (&string_table.add(unsafe_str)).into(),
            Err(_) => StringID::empty_ffi_string(),
        }
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

    pub fn dump_pprof(heap_sample_size: u64, time_range: &TimeRange) -> Option<Vec<u8>> {
        Python::attach(|_| unsafe {
            memalloc_heap_py();
        });
        let st = STRING_TABLE.lock();
        let pb = PROFILE_BUILDER.lock();
        match (st, pb) {
            (Ok(mut st), Ok(mut pb)) => {
                pb.set_memory_profile_type(st.deref_mut(), heap_sample_size);
                pb.encode_and_reset(st.deref(), time_range)
            }
            _ => None,
        }
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
}
