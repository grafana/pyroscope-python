use crate::backend::{BackendConfig, ReportBatch};
use crate::encode::pprof::ffi::FFIStringView;
use crate::error::{PyroscopeError, Result};
use std::ffi::c_int;
use std::time::Duration;

const NANOS_PER_SECOND: u64 = 1_000_000_000;
const NANOS_PER_MICROSECOND: u64 = 1_000;

#[allow(dead_code)]
pub mod ffi {
    use super::FFIStringView;

    #[repr(C)]
    pub struct FFIGcpFrame {
        pub function_name: FFIStringView,
        pub file_name: FFIStringView,
        pub line: super::c_int,
    }

    #[repr(C)]
    pub struct FFIGcpSample {
        pub frames: *const FFIGcpFrame,
        pub len: usize,
        pub count: u64,
    }
}

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
    pub fn report(&self, duration: Duration) -> Result<Option<ReportBatch>> {
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
mod implementation {
    #[cfg(test)]
    use super::ffi::FFIGcpFrame;
    use super::ffi::FFIGcpSample;
    use super::*;
    use crate::backend::ReportData;
    use crate::encode::pprof::{PProfBuilder, StringID, StringTable};
    use crate::utils::TimeRange;
    use prost::Message;
    use pyo3::prelude::*;
    use std::sync::Mutex;
    use std::time::SystemTime;

    struct ProfileState {
        builder: PProfBuilder,
        strings: StringTable,
        pid_label: Option<(StringID, StringID)>,
    }

    static PROFILE_STATE: Mutex<Option<ProfileState>> = Mutex::new(None);

    unsafe extern "C" {
        fn gcp_cpu_profiler_collect(duration_nanos: i64, period_nanos: i64) -> c_int;
    }

    fn start_profile(config: Config) -> Result<()> {
        let mut strings = StringTable::new();
        let mut builder = PProfBuilder::new();
        builder.set_cpu_profile_type(&mut strings, config.sample_rate);
        let pid_label = config.backend_config.report_pid.then(|| {
            let pid = std::process::id().to_string();
            (strings.add("pid"), strings.add_owned(pid))
        });
        *PROFILE_STATE.lock()? = Some(ProfileState {
            builder,
            strings,
            pid_label,
        });
        Ok(())
    }

    fn clear_profile() {
        if let Ok(mut state) = PROFILE_STATE.lock() {
            *state = None;
        }
    }

    fn finish_profile(time_range: &TimeRange) -> Result<Option<Vec<u8>>> {
        let Some(mut state) = PROFILE_STATE.lock()?.take() else {
            return Err(PyroscopeError::new(
                "GCP CPU profiler profile state is not initialized",
            ));
        };
        let profile = state
            .builder
            .take_profile_and_reset_owned(state.strings, time_range);
        Ok(profile.map(|profile| profile.encode_to_vec()))
    }

    unsafe fn string_from_view<'a>(data: *const std::ffi::c_char, len: usize) -> Option<&'a str> {
        if data.is_null() {
            return (len == 0).then_some("");
        }
        let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
        Some(unsafe { std::str::from_utf8_unchecked(bytes) })
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn pyroscope_gcp_push_sample(sample: FFIGcpSample) {
        if sample.frames.is_null() || sample.len == 0 {
            return;
        }
        let ffi_frames = unsafe { std::slice::from_raw_parts(sample.frames, sample.len) };
        let Ok(count) = usize::try_from(sample.count) else {
            return;
        };

        let Ok(mut guard) = PROFILE_STATE.lock() else {
            return;
        };
        let Some(state) = guard.as_mut() else {
            return;
        };
        let frames = ffi_frames.iter().map(|frame| unsafe {
            (
                string_from_view(frame.function_name.data, frame.function_name.len).unwrap_or(""),
                string_from_view(frame.file_name.data, frame.file_name.len).unwrap_or(""),
                frame.line,
            )
        });
        let labels = state.pid_label.as_slice();
        state
            .builder
            .add_cpu_sample(&mut state.strings, frames, count, labels);
    }

    pub fn collect(duration: Duration, config: Config) -> Result<Option<ReportBatch>> {
        let duration_nanos = i64::try_from(duration.as_nanos())
            .map_err(|_| PyroscopeError::new("GCP CPU profiler duration is too large"))?;
        let period_nanos = period_nanos(config.sample_rate)?;
        start_profile(config)?;
        let started = SystemTime::now();
        let status =
            Python::attach(|_| unsafe { gcp_cpu_profiler_collect(duration_nanos, period_nanos) });
        let time_range = match TimeRange::new(started, SystemTime::now()) {
            Ok(time_range) => time_range,
            Err(error) => {
                clear_profile();
                return Err(error);
            }
        };
        if status != 0 {
            clear_profile();
            return Err(PyroscopeError::new("GCP CPU profiler collection failed"));
        }

        Ok(finish_profile(&time_range)?.map(|pprof| ReportBatch {
            profile_type: "process_cpu".to_owned(),
            data: ReportData::RawPprof(pprof),
        }))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::encode::r#gen::google::Profile;
        use std::ffi::c_char;
        use std::time::{Duration, UNIX_EPOCH};

        fn string_view(value: &str) -> FFIStringView {
            FFIStringView {
                data: value.as_ptr().cast::<c_char>(),
                len: value.len(),
            }
        }

        #[test]
        fn incrementally_builds_cpu_pprof() {
            start_profile(Config {
                sample_rate: 100,
                backend_config: BackendConfig {
                    report_pid: true,
                    ..BackendConfig::default()
                },
            })
            .unwrap();

            let frames = [
                FFIGcpFrame {
                    function_name: string_view("leaf"),
                    file_name: string_view("/app/work.py"),
                    line: 42,
                },
                FFIGcpFrame {
                    function_name: string_view("root"),
                    file_name: string_view("/app/main.py"),
                    line: 7,
                },
            ];
            pyroscope_gcp_push_sample(FFIGcpSample {
                frames: frames.as_ptr(),
                len: frames.len(),
                count: 3,
            });

            let time_range =
                TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(1)).unwrap();
            let encoded = finish_profile(&time_range).unwrap().unwrap();
            let profile = Profile::decode(encoded.as_slice()).unwrap();

            assert_eq!(profile.sample.len(), 1);
            assert_eq!(profile.sample[0].value, vec![30_000_000]);
            assert_eq!(profile.sample[0].location_id.len(), 2);
            let lines: Vec<i64> = profile.sample[0]
                .location_id
                .iter()
                .map(|id| profile.location[(*id - 1) as usize].line[0].line)
                .collect();
            assert_eq!(lines, vec![42, 7]);
            assert_eq!(profile.period, 10_000_000);
            assert!(
                profile.sample[0]
                    .label
                    .iter()
                    .any(|label| profile.string_table[label.key as usize] == "pid")
            );
        }
    }
}

#[cfg(not(feature = "gcp"))]
mod implementation {
    use super::*;

    pub fn collect(_duration: Duration, _config: Config) -> Result<Option<ReportBatch>> {
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
}
