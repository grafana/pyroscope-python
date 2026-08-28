use crate::backend::{BackendConfig, ReportBatch};
#[cfg(any(feature = "gcp", test))]
use crate::backend::{Report, ReportData, StackBuffer, StackFrame, StackTrace};
use crate::error::{PyroscopeError, Result};
#[cfg(feature = "gcp")]
use pyo3::prelude::*;
#[cfg(feature = "gcp")]
use pyo3::types::{PyDict, PyTuple};
use std::time::Duration;

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_MICROSECOND: u64 = 1_000;

#[derive(Clone, Copy)]
pub struct Config {
    pub sample_rate: u32,
    pub backend_config: BackendConfig,
}

pub struct Gcp {
    config: Config,
}

#[derive(Clone, Copy)]
pub struct Reporter {
    config: Config,
}

impl Gcp {
    pub fn new(config: Config) -> Result<Self> {
        period_nanos(config.sample_rate)?;
        if !is_available() {
            return Err(PyroscopeError::new(
                "GCP CPU profiler is not available in this build",
            ));
        }
        Ok(Self { config })
    }

    pub fn reporter(&self) -> Reporter {
        Reporter {
            config: self.config,
        }
    }
}

impl Reporter {
    pub fn report(&self, duration: Duration) -> Result<ReportBatch> {
        implementation::collect(duration, self.config)
    }
}

pub fn is_available() -> bool {
    cfg!(all(feature = "gcp", target_os = "linux"))
}

pub fn period_nanos(sample_rate: u32) -> Result<i64> {
    if sample_rate == 0 {
        return Err(PyroscopeError::new(
            "GCP CPU profiler sample_rate must be greater than zero",
        ));
    }
    let period = NANOS_PER_SECOND / u64::from(sample_rate);
    if period < NANOS_PER_MICROSECOND {
        return Err(PyroscopeError::new(
            "GCP CPU profiler sample_rate must not exceed 1,000,000",
        ));
    }
    i64::try_from(period)
        .map_err(|_| PyroscopeError::new("GCP CPU profiler sample period is too large"))
}

#[cfg(feature = "gcp")]
fn traces_to_batch(
    traces: &Bound<'_, PyDict>,
    backend_config: &BackendConfig,
) -> Result<ReportBatch> {
    let mut samples = Vec::with_capacity(traces.len());
    for (trace, count) in traces.iter() {
        let frames = trace
            .cast::<PyTuple>()
            .map_err(|err| gcp_error("trace is not a tuple", err))?;
        let mut stack_frames = Vec::with_capacity(frames.len());
        for frame in frames.iter() {
            let (name, filename, line): (String, String, i32) = frame
                .extract()
                .map_err(|err| gcp_error("frame has an invalid shape", err))?;
            stack_frames.push(StackFrame {
                name,
                filename,
                line: line.max(0) as u32,
            });
        }

        let count: usize = count
            .extract()
            .map_err(|err| gcp_error("sample count is not an integer", err))?;
        samples.push((stack_frames, count));
    }

    samples_to_batch(samples, backend_config)
}

#[cfg(any(feature = "gcp", test))]
fn samples_to_batch(
    samples: impl IntoIterator<Item = (Vec<StackFrame>, usize)>,
    backend_config: &BackendConfig,
) -> Result<ReportBatch> {
    let mut buffer = StackBuffer::default();
    for (frames, count) in samples {
        let stacktrace =
            StackTrace::new(backend_config, Some(std::process::id()), None, None, frames);
        buffer.record_with_count(stacktrace, count)?;
    }

    let reports: Vec<Report> = buffer.into();
    Ok(ReportBatch {
        profile_type: "process_cpu".to_owned(),
        data: ReportData::Reports(reports),
    })
}

#[cfg(feature = "gcp")]
fn gcp_error(context: &str, error: impl std::fmt::Display) -> PyroscopeError {
    PyroscopeError::new(&format!("GCP CPU profiler {context}: {error}"))
}

#[cfg(feature = "gcp")]
mod implementation {
    use super::*;
    use pyo3::ffi;

    unsafe extern "C" {
        fn gcp_cpu_profiler_collect(duration_nanos: i64, period_nanos: i64) -> *mut ffi::PyObject;
    }

    pub fn collect(duration: Duration, config: Config) -> Result<ReportBatch> {
        let duration_nanos = i64::try_from(duration.as_nanos())
            .map_err(|_| PyroscopeError::new("GCP CPU profiler duration is too large"))?;
        let period_nanos = period_nanos(config.sample_rate)?;

        Python::attach(|py| unsafe {
            let traces = gcp_cpu_profiler_collect(duration_nanos, period_nanos);
            if traces.is_null() {
                let error = PyErr::fetch(py);
                return Err(gcp_error("collection failed", error));
            }
            let traces = Bound::from_owned_ptr(py, traces)
                .cast_into::<PyDict>()
                .map_err(|err| gcp_error("returned a non-dictionary result", err))?;
            traces_to_batch(&traces, &config.backend_config)
        })
    }
}

#[cfg(not(feature = "gcp"))]
mod implementation {
    use super::*;

    pub fn collect(_duration: Duration, _config: Config) -> Result<ReportBatch> {
        Err(PyroscopeError::new(
            "GCP CPU profiler is not available in this build",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sample_rate_is_ten_milliseconds() {
        assert_eq!(period_nanos(100).unwrap(), 10_000_000);
    }

    #[test]
    fn rejects_sample_rates_the_native_timer_cannot_represent() {
        assert!(period_nanos(0).is_err());
        assert!(period_nanos(1_000_001).is_err());
    }

    #[test]
    fn converts_leaf_first_traces_and_preserves_counts() {
        let frames = vec![
            StackFrame {
                name: "leaf".to_owned(),
                filename: "/app/work.py".to_owned(),
                line: 42,
            },
            StackFrame {
                name: "root".to_owned(),
                filename: "/app/main.py".to_owned(),
                line: 7,
            },
        ];
        let batch = samples_to_batch(
            [(frames, 3)],
            &BackendConfig {
                report_pid: true,
                ..BackendConfig::default()
            },
        )
        .unwrap();

        assert_eq!(batch.profile_type, "process_cpu");
        let ReportData::Reports(reports) = batch.data else {
            panic!("expected structured reports");
        };
        assert_eq!(reports.len(), 1);
        let (stacktrace, count) = reports[0].data.iter().next().unwrap();
        assert_eq!(*count, 3);
        assert_eq!(stacktrace.frames[0].name, "leaf");
        assert_eq!(stacktrace.frames[1].name, "root");
        assert!(reports[0].metadata.tags.iter().any(|tag| tag.key == "pid"));
    }
}
