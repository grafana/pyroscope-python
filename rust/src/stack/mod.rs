use crate::encode::pprof::ffi::{FFIFrame, FFISampleValues};
use crate::encode::pprof::{CpuWallProfile, PProfBuilder};
use crate::forksafety::LeakableMutex;
use crate::utils::TimeRange;
use prost::Message;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

mod asyncio;
mod faulthandler;
mod sampler;
mod threads;

use sampler::{
    bump_upload_seq, configure, fast_copy_initialized, interval_us, is_safe_copy_failed,
    sampler_start, sampler_stop, sampling_thread_failed,
};
static PROFILE_BUILDER: LeakableMutex<PProfBuilder<CpuWallProfile>> = LeakableMutex::new();

#[derive(Clone)]
pub struct Config {
    pub enabled: bool,
}

#[derive(Debug)]
pub struct Options {
    pub fast_copy: bool,
    pub fast_copy_warmup_s: f64,
    pub max_nframe: u32,
    pub max_threads: u32,
    pub max_tasks: u32,
    pub adaptive_sampling: bool,
    pub adaptive_target_overhead: f64,
    pub adaptive_max_interval_us: u64,
    pub adaptive_baseline: f64,
    pub adaptive_p_stable_window_s: u32,
    pub adaptive_p_stable_percentile: f64,
    pub async_tracking: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fast_copy: true,
            fast_copy_warmup_s: 15.0,
            max_nframe: 128,
            max_threads: 25,
            max_tasks: 50,
            adaptive_sampling: false,
            adaptive_target_overhead: 0.01,
            adaptive_max_interval_us: 1_000_000,
            adaptive_baseline: 0.0,
            adaptive_p_stable_window_s: 600,
            adaptive_p_stable_percentile: 95.0,
            async_tracking: false,
        }
    }
}

/// Tracks whether `pyroscope_stack_start` succeeded, so `stop` never calls
/// `Sampler::stop()` on a sampler that was never started. That call bumps
/// `thread_seq_num` unconditionally, and `Sampler::prefork` reads the
/// counter's *parity* to decide whether to restart after a fork.
static STARTED: AtomicBool = AtomicBool::new(false);

static OPTIONS: OnceLock<Options> = OnceLock::new();

pub fn set_options(options: Options) -> bool {
    set_options_in(&OPTIONS, options)
}

fn set_options_in(lock: &OnceLock<Options>, options: Options) -> bool {
    if lock.set(options).is_err() {
        log::warn!(
            target: "pyroscope-python",
            "ignoring configure_cpu_profiler: the CPU profiler options are already fixed for \
             this process, by an earlier call or by the session that started the sampler; \
             in effect: {:?}",
            lock.get()
        );
        return false;
    }
    true
}

fn options() -> &'static Options {
    OPTIONS.get_or_init(Options::default)
}

/// Start the vendored echion sampler, then register the live threads with it.
///
/// Mirrors the order of `ddtrace/profiling/collector/stack.py::_init`: setters,
/// `is_safe_copy_failed()`, `start()`, thread registration, then the asyncio
/// patch. Registration must come after `start()` -- `Sampler::start` runs
/// `one_time_setup`, which placement-news echion's thread info map and would
/// discard any earlier registration -- and before the asyncio patch, whose
/// `track_asyncio_loop` is dropped for an unregistered thread.
pub fn start(py: Python<'_>, config: &Config, sample_rate: u32) -> PyResult<()> {
    if !config.enabled {
        return Ok(());
    }

    let options = options();
    configure(1.0 / f64::from(sample_rate.max(1)), options);

    if is_safe_copy_failed() {
        log::error!(
            target: "pyroscope-python",
            "no safe memory copy method available (safe_memcpy and process_vm_readv both failed); \
             the CPU stack sampler stays off"
        );
        return Ok(());
    }

    if fast_copy_initialized() {
        faulthandler::install(py)?;
    }

    if !sampler_start() {
        return Err(PyRuntimeError::new_err(
            "failed to start the CPU stack sampler's sampling thread",
        ));
    }
    STARTED.store(true, Ordering::Release);

    threads::install(py)?;
    if options.async_tracking {
        asyncio::install(py);
    }
    Ok(())
}

pub fn stop(py: Python<'_>) {
    if STARTED.swap(false, Ordering::AcqRel) {
        // Not under the GIL: Sampler::stop waits for the sampling thread, which
        // interns strings, and `memory::dump_pprof` holds the interner lock
        // while attached to Python.
        py.detach(sampler_stop);
    }
    clear_samples();
}

pub fn postfork_child() {
    #[cfg(not(miri))]
    PROFILE_BUILDER.leak_and_reset();
    #[cfg(miri)]
    let _ = PROFILE_BUILDER.leak_and_reset();
}

pub fn push_sample(frames: &[FFIFrame], values: &FFISampleValues) {
    if let Ok(mut pb) = PROFILE_BUILDER.mutex().lock() {
        pb.add_ffi_sample(frames, values);
    }
}

/// Discard the samples buffered for the next cpu/wall profile.
///
/// See `crate::memory::implementation::clear_samples` for the reasoning,
/// including why the shared string table is deliberately left alone.
pub fn clear_samples() {
    let mut pb = PROFILE_BUILDER
        .mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    pb.reset();
}

/// Take the accumulated cpu/wall samples as an encoded pprof, if any.
///
/// Unlike `crate::memory::dump_pprof` this needs neither the GIL nor a
/// profiler-side flush, but it keeps the same lock order: the interner before
/// the profile builder, never the reverse.
pub fn dump_pprof(sample_rate: u32, time_range: &TimeRange) -> Option<Vec<u8>> {
    let st = crate::encode::interner::string_table().lock();
    let pb = PROFILE_BUILDER.mutex().lock();
    let profile = match (st, pb) {
        (Ok(mut st), Ok(mut pb)) => {
            pb.set_profile_type(st.deref_mut(), period_ns(sample_rate));
            pb.take_profile_and_reset(st.deref(), time_range)
        }
        _ => None,
    }?;
    bump_upload_seq();
    Some(profile.encode_to_vec())
}

pub fn report_sampling_thread_error() {
    if STARTED.load(Ordering::Acquire) && sampling_thread_failed() {
        log::error!(
            target: "pyroscope-python",
            "the CPU stack sampler's sampling thread died and stopped sampling; \
             its cpu/wall profiles are empty from here on"
        );
    }
}

/// Asks the sampler rather than restating `1 / sample_rate`, because adaptive
/// sampling moves the real interval. It reports 0 before the first
/// `configure()` and under miri.
fn period_ns(sample_rate: u32) -> i64 {
    match interval_us() {
        0 => 1_000_000_000 / i64::from(sample_rate.max(1)),
        us => (us as i64).saturating_mul(1_000),
    }
}

/// The test binary links no libpython: pyo3's `extension-module` leaves every
/// Python symbol to the host process, and `--gc-sections` drops the vendored
/// C++ that needs them. `Sampler::track_asyncio_loop` is the exception.
#[cfg(test)]
#[unsafe(no_mangle)]
static _Py_NoneStruct: [usize; 4] = [0; 4];

#[cfg(all(test, not(miri)))]
mod thread_registration_tests;

#[cfg(test)]
mod tests;
