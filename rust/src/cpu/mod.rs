//! CPU profiler selection.
//!
//! py-spy is the default and works everywhere. `Ddtrace` is a vendored native
//! sampler under `cpp/cpu/ddtrace_stack/` (dd-trace-py's echion-based
//! wall-clock sampler), driven over a C ABI.
//!
//! Both report through the same `StackBuffer` -> `Report` -> `encode::pprof`
//! -> `session` path, so switching implementations changes what is sampled and
//! how much it costs, not how the result is encoded or uploaded.

pub mod native;

use crate::backend::{BackendConfig, Report, ReportBatch, ReportData, ThreadTagsSet};
use crate::error::Result;
use crate::pyspy_backend::Pyspy;
use pyo3::prelude::*;

/// Which CPU profiler implementation to run.
#[pyclass(eq, eq_int, from_py_object)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CpuProfiler {
    /// py-spy: reads CPython structures out of this process's own memory from a
    /// background Rust thread. Works on every supported platform.
    #[default]
    PySpy = 0,
    /// dd-trace-py's `stack` sampler (echion): a dedicated sampler thread walks
    /// every Python thread on a wall-clock interval and weights each stack by
    /// that thread's CPU delta. See `cpp/cpu/ddtrace_stack/VENDOR.md`.
    Ddtrace = 1,
}

impl CpuProfiler {
    pub fn name(&self) -> &'static str {
        match self {
            CpuProfiler::PySpy => "pyspy",
            CpuProfiler::Ddtrace => "ddtrace",
        }
    }

    /// Why this profiler cannot run here, or `Ok(())` if it can.
    ///
    /// Callers surface this as an error rather than silently falling back to
    /// py-spy: a silent fallback would report one implementation's profile
    /// under another's name.
    pub fn check_supported(&self, py: Python<'_>) -> std::result::Result<(), String> {
        match self {
            CpuProfiler::PySpy => Ok(()),
            CpuProfiler::Ddtrace => {
                if !native::ddtrace_built() {
                    return Err(format!(
                        "cpu_profiler={} is not available in this build (platform {}/{}); \
                         it is currently built for Linux only, and not for free-threaded \
                         interpreters",
                        self.name(),
                        std::env::consts::OS,
                        std::env::consts::ARCH,
                    ));
                }
                // It discovers threads by walking the interpreter's thread list
                // and deriving each thread's CPU clock from the kernel TID in
                // PyThreadState::native_thread_id, which only exists from
                // CPython 3.11 on. Without it the thread map stays empty and
                // the profile would come back blank, so refuse up front.
                let v = py.version_info();
                if (v.major, v.minor) < (3, 11) {
                    return Err(format!(
                        "cpu_profiler={} requires CPython 3.11 or newer (running {}.{}); \
                         it needs PyThreadState::native_thread_id to discover threads",
                        self.name(),
                        v.major,
                        v.minor
                    ));
                }
                Ok(())
            }
        }
    }
}

/// Everything needed to start a CPU profiler, whichever one was selected.
#[derive(Clone)]
pub struct CpuConfig {
    pub profiler: CpuProfiler,
    pub sample_rate: u32,
    pub backend_config: BackendConfig,
    /// py-spy's own config. Only consulted for [`CpuProfiler::PySpy`], except
    /// that the native path reads `include_idle`/`gil_only` to warn about knobs
    /// it cannot honour.
    pub pyspy: py_spy::config::Config,
}

/// A running CPU profiler.
///
/// `Pyspy` is boxed because it is far larger than `NativeCpu` (which holds only
/// a discriminant and a flag -- the native sampler keeps its state in C++ and in
/// one global buffer), and the agent stores this enum by value.
pub enum CpuBackend {
    PySpy(Box<Pyspy>),
    Native(native::NativeCpu),
}

impl CpuBackend {
    pub fn new(config: CpuConfig, ruleset: ThreadTagsSet) -> Result<Self> {
        match config.profiler {
            CpuProfiler::PySpy => Ok(CpuBackend::PySpy(Box::new(Pyspy::new(
                config.pyspy,
                config.backend_config,
                ruleset,
            )?))),
            other => Ok(CpuBackend::Native(native::NativeCpu::start(
                other, &config, ruleset,
            )?)),
        }
    }

    pub fn reporter(&self) -> CpuReporter {
        match self {
            CpuBackend::PySpy(pyspy) => CpuReporter::PySpy(pyspy.reporter()),
            CpuBackend::Native(_) => CpuReporter::Native,
        }
    }

    pub fn shutdown_thread(&mut self) -> Result<()> {
        match self {
            CpuBackend::PySpy(pyspy) => pyspy.shutdown_thread(),
            CpuBackend::Native(native) => native.shutdown(),
        }
    }
}

/// Drain handle used by the agent's snapshot loop.
pub enum CpuReporter {
    PySpy(crate::pyspy_backend::Reporter),
    /// The native sampler pushes into one global buffer, so there is nothing
    /// per-instance to carry here.
    Native,
}

impl CpuReporter {
    pub fn report(&self) -> Result<ReportBatch> {
        match self {
            CpuReporter::PySpy(reporter) => reporter.report(),
            CpuReporter::Native => {
                let buffer = native::take_buffer()?;
                let reports: Vec<Report> = buffer.into();
                Ok(ReportBatch {
                    profile_type: "process_cpu".into(),
                    // Values are CPU nanoseconds already, not tick counts.
                    data: ReportData::ReportsCpuNanos(reports),
                })
            }
        }
    }
}

/// Disarm the native sampler in a freshly forked child.
///
/// The agent is leaked wholesale in the child (its threads did not survive the
/// fork), so `CpuBackend::drop` never runs and cannot do this.
pub fn postfork_child() {
    native::postfork_child();
}
