use crate::utils::TimeRange;
use pyo3::prelude::*;

#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
    pub enable_mem_domain: bool,
    pub max_nframe: u16,
    pub heap_sample_size: u64,
}

pub fn start(_py: Python<'_>, config: &Config) {
    if !config.enabled {
        return;
    }
    implementation::ensure_initialized();
    unsafe {
        implementation::memalloc_start(
            config.max_nframe,
            config.heap_sample_size,
            config.enable_mem_domain,
        )
    }
}

pub fn stop(_py: Python<'_>) {
    unsafe {
        implementation::memalloc_stop();
    }
}

pub fn dump_pprof(
    _py: Python<'_>,
    heap_sample_size: u64,
    time_range: &TimeRange,
) -> Option<Vec<u8>> {
    implementation::dump_pprof(_py, heap_sample_size, time_range)
}

mod implementation {
    use crate::encode::pprof::PProfBuilder;
    use crate::encode::pprof::ffi::{FFIInternedString, FFISample, FFIStringView};
    use crate::encode::pprof::{StringID, StringTable};
    use crate::utils::TimeRange;
    use lazy_static::lazy_static;
    use pyo3::prelude::*;
    use std::cell::UnsafeCell;
    use std::ops::{Deref, DerefMut};
    use std::sync::{Mutex, MutexGuard};

    lazy_static! {
        static ref STRING_TABLE: Mutex<StringTable> = Mutex::new(StringTable::new());
    }

    lazy_static! {
        static ref PROFILE_BUILDER: Mutex<PProfBuilder> = Mutex::new(PProfBuilder::new());
    }

    struct AtForkGuards {
        string_table: Option<MutexGuard<'static, StringTable>>,
        profile_builder: Option<MutexGuard<'static, PProfBuilder>>,
    }

    struct AtForkGuardStore(UnsafeCell<AtForkGuards>);

    // pthread_atfork runs these callbacks around a single fork operation. The
    // prepare callback stores guards here so the parent and child callbacks can
    // release the same locks without exposing this state to regular profiler
    // code.
    unsafe impl Sync for AtForkGuardStore {}

    static ATFORK_GUARDS: AtForkGuardStore = AtForkGuardStore(UnsafeCell::new(AtForkGuards {
        string_table: None,
        profile_builder: None,
    }));

    unsafe extern "C" {
        pub fn memalloc_start(max_nframe: u16, heap_sample_size: u64, enable_mem_domain: bool);
        pub fn memalloc_stop();
        // flush heap inuse samples
        pub fn memalloc_heap_py();
    }

    pub fn ensure_initialized() {
        lazy_static::initialize(&STRING_TABLE);
        lazy_static::initialize(&PROFILE_BUILDER);
    }

    fn lock_unpoisoned<T>(mutex: &'static Mutex<T>) -> MutexGuard<'static, T> {
        match mutex.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    unsafe fn atfork_guards_mut() -> &'static mut AtForkGuards {
        unsafe { &mut *ATFORK_GUARDS.0.get() }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn pyroscope_memprof_atfork_prepare() {
        let string_table = lock_unpoisoned(&STRING_TABLE);
        let profile_builder = lock_unpoisoned(&PROFILE_BUILDER);

        let guards = unsafe { atfork_guards_mut() };
        debug_assert!(guards.string_table.is_none());
        debug_assert!(guards.profile_builder.is_none());
        guards.string_table = Some(string_table);
        guards.profile_builder = Some(profile_builder);
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn pyroscope_memprof_atfork_parent() {
        let guards = unsafe { atfork_guards_mut() };
        let profile_builder = guards.profile_builder.take();
        let string_table = guards.string_table.take();
        drop(profile_builder);
        drop(string_table);
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn pyroscope_memprof_atfork_child() {
        let guards = unsafe { atfork_guards_mut() };
        let mut profile_builder = guards.profile_builder.take();
        let mut string_table = guards.string_table.take();

        if let Some(guard) = profile_builder.as_mut() {
            **guard = PProfBuilder::new();
        }
        if let Some(guard) = string_table.as_mut() {
            **guard = StringTable::new();
        }

        PROFILE_BUILDER.clear_poison();
        STRING_TABLE.clear_poison();

        drop(profile_builder);
        drop(string_table);
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

    pub fn dump_pprof(
        _py: Python<'_>,
        heap_sample_size: u64,
        time_range: &TimeRange,
    ) -> Option<Vec<u8>> {
        unsafe {
            memalloc_heap_py();
        }
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
