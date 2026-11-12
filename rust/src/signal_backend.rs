use crate::{
    backend::BackendConfig,
    encode::{
        r#gen::google::Profile,
        pprof::{FunctionMirror, PProfBuilder, PySpyProfile, StringTable, ffi::FFIFrame},
    },
    error::{PyroscopeError, Result},
    pyspy_backend::Reporter,
    signal_sampler::{self, SharedTimer, SignalHandlerSession, SignalTimer},
    utils::TimeRange,
};
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

const LOG_TAG: &str = "Pyroscope::Signal";

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub sampler: signal_sampler::Config,
}

pub struct Signal {
    backend_config: BackendConfig,
    buffer: Arc<Mutex<ProfileBuffer>>,
    sampler_thread: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Signal {
    pub fn new(config: Config, backend_config: BackendConfig) -> Result<Self> {
        let mut session = SignalHandlerSession::start(&config.sampler)
            .map_err(|e| PyroscopeError::new(&format!("Signal: {e}")))?;
        if config.sampler.signal_timer == SignalTimer::CTimer {
            threads::attach(session.timer())?;
        }

        let buffer = Arc::new(Mutex::new(ProfileBuffer::new()));
        let running = Arc::new(AtomicBool::new(true));
        let sampler_thread = {
            let buffer = buffer.clone();
            let running = running.clone();
            std::thread::spawn(move || {
                while running.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10)); //todo do not wake up on every 10ms, notify from producer when buf is half full
                    let Ok(mut buffer) = buffer.lock() else {
                        break;
                    };
                    let ProfileBuffer {
                        builder, strings, ..
                    } = &mut *buffer;
                    // TODO: Apply thread tags once samples include the thread ID.
                    session.drain(strings, |frames, count| add_sample(builder, frames, count));
                }
                if config.sampler.signal_timer == SignalTimer::CTimer {
                    threads::detach();
                }
                drop(session);
            })
        };

        Ok(Signal {
            backend_config,
            buffer,
            sampler_thread: Some(sampler_thread),
            running,
        })
    }

    pub fn reporter(&self) -> Reporter {
        Reporter::new_signal(self.buffer.clone(), self.backend_config)
    }

    pub fn shutdown_thread(&mut self) -> Result<()> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.sampler_thread.take() {
            handle
                .join()
                .map_err(|_| PyroscopeError::new("Signal: Failed to join sampler thread"))?;
        }
        Ok(())
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        let _ = self.shutdown_thread();
    }
}

/// Incremental signal profile. String IDs, functions, locations and accumulated
/// samples are scoped to one upload window.
pub struct ProfileBuffer {
    builder: PProfBuilder<PySpyProfile>,
    strings: StringTable,
    drop_name: String,
}

impl ProfileBuffer {
    pub fn new() -> Self {
        Self {
            builder: PProfBuilder::new(),
            strings: StringTable::new(),
            drop_name: String::new(),
        }
    }

    fn record_dropped_stacks(&mut self, counts: impl IntoIterator<Item = (&'static str, usize)>) {
        use std::fmt::Write;
        // Reuse formatting storage across drop reasons and upload windows.
        for (reason, count) in counts {
            if count == 0 {
                continue;
            }
            self.drop_name.clear();
            write!(&mut self.drop_name, "[dropped: {reason}]").unwrap();
            let frame = FFIFrame {
                function_name: (&self.strings.add(&self.drop_name)).into(),
                file_name: (&self.strings.add("")).into(),
                line: 0,
            };
            add_sample(&mut self.builder, &[frame], count);
        }
    }

    pub fn report(&mut self, sample_rate: u32, time_range: &TimeRange) -> Option<Profile> {
        // Consume drops only when the corresponding upload window is built.
        self.record_dropped_stacks(pyroscope_python_unwind::METRICS.take_drop_counts());
        self.take_profile(sample_rate, time_range)
    }

    fn take_profile(&mut self, sample_rate: u32, time_range: &TimeRange) -> Option<Profile> {
        self.builder
            .set_profile_type(&mut self.strings, sample_rate);
        let profile = self
            .builder
            .take_profile_and_reset(&self.strings, time_range);
        self.strings = StringTable::new();
        let mut profile = profile?;
        // Accumulate sampling periods, including timer overruns, until the upload.
        for sample in &mut profile.sample {
            sample.value[0] = sample.value[0].saturating_mul(profile.period);
        }
        Some(profile)
    }
}

fn add_sample(builder: &mut PProfBuilder<PySpyProfile>, frames: &[FFIFrame], count: usize) {
    builder.add_sample(
        frames.iter().map(|frame| {
            (
                FunctionMirror {
                    name: (&frame.function_name).into(),
                    filename: (&frame.file_name).into(),
                },
                i64::from(frame.line.max(0)),
            )
        }),
        [i64::try_from(count).unwrap_or(i64::MAX)],
    );
}

/// Feeds the OS thread ids of python threads to a `CTimer` session. The hooks stay installed
/// for the life of the process and keep the live threads in `LIVE`, so a session that starts
/// later still learns about the threads that started before it.
pub mod threads {
    use super::*;

    static INSTALLED: OnceLock<()> = OnceLock::new();
    static PROCESS_ID: AtomicU32 = AtomicU32::new(0);
    static LIVE: Mutex<Option<HashMap<u64, libc::pid_t>>> = Mutex::new(None);
    static TIMER: Mutex<Option<SharedTimer>> = Mutex::new(None);

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[pyfunction]
    fn register_thread(id: u64, native_id: u64) {
        let tid = native_id as libc::pid_t;
        let previous = lock(&LIVE).get_or_insert_default().insert(id, tid);
        if let Some(timer) = lock(&TIMER).as_ref() {
            if let Some(previous) = previous.filter(|&previous| previous != tid) {
                signal_sampler::thread_removed(timer, previous);
            }
            if let Err(e) = signal_sampler::thread_created(timer, tid) {
                log::warn!(target: LOG_TAG, "{e}");
            }
        }
    }

    #[pyfunction]
    fn unregister_thread(id: u64) {
        let Some(tid) = lock(&LIVE).get_or_insert_default().remove(&id) else {
            return;
        };
        if let Some(timer) = lock(&TIMER).as_ref() {
            signal_sampler::thread_removed(timer, tid);
        }
    }

    pub fn install(py: Python<'_>) -> PyResult<()> {
        if INSTALLED.get().is_some() {
            if PROCESS_ID.load(Ordering::Relaxed) != std::process::id() {
                // shutdown-before-fork leaves no active timer, but native thread IDs change.
                // The Python wrappers survive fork; refresh their registry without wrapping again.
                lock(&LIVE).get_or_insert_default().clear();
                py.import("pyroscope")?
                    .getattr("_register_stack_threads")?
                    .call1((
                        py.import("threading")?,
                        wrap_pyfunction!(register_thread, py)?,
                    ))?;
                PROCESS_ID.store(std::process::id(), Ordering::Relaxed);
            }
            return Ok(());
        }
        py.import("pyroscope")?
            .getattr("_install_stack_threads")?
            .call1((
                py.import("threading")?,
                wrap_pyfunction!(register_thread, py)?,
                wrap_pyfunction!(unregister_thread, py)?,
            ))?;
        PROCESS_ID.store(std::process::id(), Ordering::Relaxed);
        let _ = INSTALLED.set(());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn attach(timer: SharedTimer) -> Result<()> {
        if INSTALLED.get().is_none() {
            return Err(PyroscopeError::new(
                "Signal: the thread hooks of the CTimer are not installed",
            ));
        }
        let id = unsafe { pyo3::ffi::PyThread_get_thread_ident() };
        let caller_tid = pyroscope_signal_timer::gettid();
        let mut slot = lock(&TIMER);
        let mut live = lock(&LIVE);
        let live = live.get_or_insert_default();
        if let Some(previous) = live.insert(id, caller_tid).filter(|&tid| tid != caller_tid) {
            log::warn!(
                target: LOG_TAG,
                "replacing registered native thread id {previous} with calling thread id {caller_tid}; \
                 the process probably forked without Python's fork hooks \
                 (for example uWSGI without --py-call-uwsgi-fork-hooks)"
            );
        }
        signal_sampler::thread_created(&timer, caller_tid).map_err(|e| {
            PyroscopeError::new(&format!(
                "Signal: failed to arm the configure caller's CPU timer: {e}"
            ))
        })?;
        for &tid in live.values() {
            if tid != caller_tid
                && let Err(e) = signal_sampler::thread_created(&timer, tid)
            {
                log::warn!(target: LOG_TAG, "{e}");
            }
        }
        *slot = Some(timer);
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub(super) fn attach(_timer: SharedTimer) -> Result<()> {
        Err(PyroscopeError::new("Signal: CTimer works only on Linux"))
    }

    pub(super) fn detach() {
        lock(&TIMER).take();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{encode::r#gen::google::Profile, utils::TimeRange};
    use prost::Message;
    use std::time::{Duration, UNIX_EPOCH};

    #[cfg(all(not(miri), not(Py_3_15)))]
    pub(crate) fn profile_for_resolved_stack(
        frames: &[FFIFrame],
        strings: StringTable,
        counts: &[usize],
    ) -> Profile {
        let mut buffer = ProfileBuffer {
            builder: PProfBuilder::new(),
            strings,
            drop_name: String::new(),
        };
        for &count in counts {
            add_sample(&mut buffer.builder, frames, count);
        }
        let range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(1)).unwrap();
        let bytes = buffer.take_profile(100, &range).unwrap().encode_to_vec();
        Profile::decode(bytes.as_slice()).unwrap()
    }

    pub(crate) fn profile_frames<'a>(
        profile: &'a Profile,
        sample: &crate::encode::r#gen::google::Sample,
    ) -> Vec<(&'a str, &'a str, i64)> {
        sample
            .location_id
            .iter()
            .rev()
            .flat_map(|id| {
                let location = profile
                    .location
                    .iter()
                    .find(|location| location.id == *id)
                    .unwrap();
                location.line.iter().map(|line| {
                    let function = profile
                        .function
                        .iter()
                        .find(|function| function.id == line.function_id)
                        .unwrap();
                    (
                        profile.string_table[function.name as usize].as_str(),
                        profile.string_table[function.filename as usize].as_str(),
                        line.line,
                    )
                })
            })
            .collect()
    }

    #[test]
    fn drop_reasons_are_single_frame_pprof_samples_with_exact_weights() {
        let mut buffer = ProfileBuffer::new();
        let frames = [
            FFIFrame {
                function_name: (&buffer.strings.add("work")).into(),
                file_name: (&buffer.strings.add("workload.py")).into(),
                line: 20,
            },
            FFIFrame {
                function_name: (&buffer.strings.add("main")).into(),
                file_name: (&buffer.strings.add("workload.py")).into(),
                line: 10,
            },
        ];
        add_sample(&mut buffer.builder, &frames, 40);
        add_sample(&mut buffer.builder, &frames, 60);
        buffer.record_dropped_stacks([("resolve_error", 0)]);
        buffer.record_dropped_stacks([
            ("contentions", 1),
            ("buffer_dropped", 2),
            ("no_thread_state", 3),
            ("invalid_code_type", 4),
            ("read_errors", 5),
            ("truncated", 13),
            ("resolve_error", 9),
        ]);
        let range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();
        let bytes = buffer.take_profile(100, &range).unwrap().encode_to_vec();
        let profile = Profile::decode(bytes.as_slice()).unwrap();
        let mut actual: Vec<_> = profile
            .sample
            .iter()
            .map(|sample| {
                let frames = profile_frames(&profile, sample);
                assert!(sample.label.is_empty(), "{profile:#?}");
                (frames, sample.value.clone())
            })
            .collect();
        actual.sort();
        let mut expected = vec![
            (
                vec![("main", "workload.py", 10), ("work", "workload.py", 20)],
                vec![1_000_000_000],
            ),
            (vec![("[dropped: contentions]", "", 0)], vec![10_000_000]),
            (vec![("[dropped: buffer_dropped]", "", 0)], vec![20_000_000]),
            (
                vec![("[dropped: no_thread_state]", "", 0)],
                vec![30_000_000],
            ),
            (
                vec![("[dropped: invalid_code_type]", "", 0)],
                vec![40_000_000],
            ),
            (vec![("[dropped: read_errors]", "", 0)], vec![50_000_000]),
            (vec![("[dropped: truncated]", "", 0)], vec![130_000_000]),
            (vec![("[dropped: resolve_error]", "", 0)], vec![90_000_000]),
        ];
        expected.sort();
        assert_eq!(actual, expected, "{profile:#?}");
        assert_eq!(
            profile
                .sample
                .iter()
                .map(|sample| sample.value[0])
                .sum::<i64>(),
            1_370_000_000,
            "{profile:#?}"
        );
    }

    #[test]
    fn empty_upload_windows_release_unreferenced_strings() {
        let mut buffer = ProfileBuffer::new();
        let empty_memory = buffer.strings.memory_size_bytes();
        let range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(1)).unwrap();
        buffer.strings.add(&"orphaned filename".repeat(1000));
        assert!(buffer.take_profile(100, &range).is_none());
        assert_eq!(buffer.strings.set.len(), 1);
        assert_eq!(buffer.strings.memory_size_bytes(), empty_memory);
        assert_eq!(buffer.strings.add("").index, 0);
    }

    #[test]
    fn upload_windows_reclaim_strings_and_keep_profile_size_constant() {
        let mut buffer = ProfileBuffer::new();
        let empty_memory = buffer.strings.memory_size_bytes();
        let mut encoded_size = None;
        for window in 0..16 {
            let rate = if window % 2 == 0 { 100 } else { 200 };
            let filename_text = format!("source_{window:03}.py");
            let range = TimeRange::new(
                UNIX_EPOCH + Duration::from_secs(5),
                UNIX_EPOCH + Duration::from_secs(15),
            )
            .unwrap();
            assert!(buffer.take_profile(rate, &range).is_none());
            let filename = buffer.strings.add(&filename_text);
            let name = buffer.strings.add("work");
            let root = buffer.strings.add("main");
            let mut stack = [
                FFIFrame {
                    function_name: (&name).into(),
                    file_name: (&filename).into(),
                    line: -1,
                },
                FFIFrame {
                    function_name: (&root).into(),
                    file_name: (&filename).into(),
                    line: 10,
                },
            ];
            stack[0].line = -1;
            add_sample(&mut buffer.builder, &stack, 2);
            add_sample(&mut buffer.builder, &stack, 3);
            stack[0].line = 25;
            add_sample(&mut buffer.builder, &stack, 7);
            let bytes = buffer.take_profile(rate, &range).unwrap().encode_to_vec();
            let profile = Profile::decode(bytes.as_slice()).unwrap();
            let mut actual: Vec<_> = profile
                .sample
                .iter()
                .map(|sample| {
                    assert!(sample.label.is_empty(), "{profile:#?}");
                    (profile_frames(&profile, sample), sample.value.clone())
                })
                .collect();
            actual.sort();
            let period = 1_000_000_000 / i64::from(rate);
            assert_eq!(
                actual,
                [
                    (
                        vec![
                            ("main", filename_text.as_str(), 10),
                            ("work", filename_text.as_str(), 0)
                        ],
                        vec![5 * period]
                    ),
                    (
                        vec![
                            ("main", filename_text.as_str(), 10),
                            ("work", filename_text.as_str(), 25)
                        ],
                        vec![7 * period]
                    ),
                ],
                "{profile:#?}"
            );
            assert_eq!(profile.function.len(), 2, "{profile:#?}");
            assert_eq!(profile.location.len(), 3, "{profile:#?}");
            assert_eq!(profile.period, period, "{profile:#?}");
            assert_eq!(profile.time_nanos, 5_000_000_000, "{profile:#?}");
            assert_eq!(profile.duration_nanos, 10_000_000_000, "{profile:#?}");
            assert_eq!(profile.sample_type.len(), 1, "{profile:#?}");
            let sample_type = &profile.sample_type[0];
            assert_eq!(
                (
                    &*profile.string_table[sample_type.r#type as usize],
                    &*profile.string_table[sample_type.unit as usize]
                ),
                ("cpu", "nanoseconds"),
                "{profile:#?}"
            );
            assert_eq!(
                profile.period_type.as_ref(),
                Some(sample_type),
                "{profile:#?}"
            );
            assert_eq!(buffer.strings.set.len(), 1);
            assert_eq!(buffer.strings.memory_size_bytes(), empty_memory);
            assert_eq!(buffer.strings.add("").index, 0);
            assert_eq!(
                profile.string_table,
                [
                    "",
                    filename_text.as_str(),
                    "work",
                    "main",
                    "cpu",
                    "nanoseconds"
                ],
                "{profile:#?}"
            );
            let expected_size = *encoded_size.get_or_insert(bytes.len());
            assert_eq!(bytes.len(), expected_size, "window {window}: {profile:#?}");
            assert!(buffer.take_profile(rate, &range).is_none());
        }
    }
}
