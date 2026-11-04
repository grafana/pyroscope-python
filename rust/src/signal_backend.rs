use crate::{
    backend::{BackendConfig, StackBuffer, StackFrame, StackTrace},
    error::{PyroscopeError, Result},
    pyspy_backend::Reporter,
    signal_sampler::{self, ResolvedFrame, SignalHandlerSession, SignalTimer},
};
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use pyroscope_signal_timer::{Mode, SignalHandler, Timer};
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

/// Shared with thread hooks; cleared by `Signal` before its handler is released.
type SharedTimer = Arc<Mutex<Option<Timer>>>;

pub struct Signal {
    timer: SharedTimer,
    handler: Option<SignalHandler>,
    buffer: Arc<Mutex<StackBuffer>>,
    backend_config: BackendConfig,
    sampler_thread: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Signal {
    pub fn new(config: Config, backend_config: BackendConfig) -> Result<Self> {
        let (mut session, handler) = SignalHandlerSession::start(&config.sampler)
            .map_err(|e| PyroscopeError::new(&format!("Signal: {e}")))?;
        let mode = match config.sampler.signal_timer {
            SignalTimer::ITimer => Mode::ITimer,
            #[cfg(target_os = "linux")]
            SignalTimer::CTimer => Mode::CTimer,
            #[cfg(not(target_os = "linux"))]
            SignalTimer::CTimer => {
                return Err(PyroscopeError::new("Signal: CTimer works only on Linux"));
            }
        };
        let interval = Duration::from_secs_f64(1.0 / config.sampler.sampling_rate as f64);
        let timer = Timer::start(mode, interval)
            .map_err(|e| PyroscopeError::new(&format!("Signal: failed to start the timer: {e}")))?;
        let mut signal = Signal {
            timer: Arc::new(Mutex::new(Some(timer))),
            handler: Some(handler),
            buffer: Arc::new(Mutex::new(StackBuffer::default())),
            backend_config,
            sampler_thread: None,
            running: Arc::new(AtomicBool::new(true)),
        };
        // From here, any startup error uses Signal's shutdown ordering.
        if config.sampler.signal_timer == SignalTimer::CTimer {
            threads::attach(signal.timer.clone())?;
        }

        let buffer = signal.buffer.clone();
        let running = signal.running.clone();
        signal.sampler_thread = Some(
            std::thread::Builder::new()
                .spawn(move || {
                    let pid = std::process::id();
                    while running.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(10)); //todo do not wake up on every 10ms, notify from producer when buf is half full
                        let traces = session.drain();
                        let Ok(mut buffer) = buffer.lock() else {
                            break;
                        };
                        for frames in traces {
                            let trace = to_stack_trace(frames, &backend_config, pid);
                            // TODO: Apply thread tags once samples include the thread ID.
                            let _ = buffer.record(trace);
                        }
                    }
                })
                .map_err(|e| {
                    PyroscopeError::new(&format!("Signal: failed to start sampler thread: {e}"))
                })?,
        );

        Ok(signal)
    }

    pub fn reporter(&self) -> Reporter {
        Reporter::new_signal(self.buffer.clone(), self.backend_config)
    }

    pub fn shutdown_thread(&mut self) -> Result<()> {
        let Some(handler) = self.handler.take() else {
            return Ok(());
        };
        threads::detach();
        // Stop all timers under the shared lock before joining the resolver or releasing
        // the handler. A hook already using the timer must finish first.
        *self.timer.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.sampler_thread.take() {
            handle
                .join()
                .map_err(|_| PyroscopeError::new("Signal: Failed to join sampler thread"))?;
        }
        drop(handler);
        Ok(())
    }
}

impl Drop for Signal {
    fn drop(&mut self) {
        let _ = self.shutdown_thread();
    }
}

pub(super) fn record_dropped_stacks(
    buffer: &mut StackBuffer,
    counts: impl IntoIterator<Item = (&'static str, usize)>,
    backend_config: &BackendConfig,
    pid: u32,
) -> Result<()> {
    for (reason, count) in counts {
        if count == 0 {
            continue;
        }
        let trace = StackTrace::new(
            backend_config,
            Some(pid),
            None,
            None,
            vec![StackFrame {
                name: format!("[dropped: {reason}]"),
                ..StackFrame::default()
            }],
        );
        buffer.record_with_count(trace, count)?;
    }
    Ok(())
}

/// The handler does not record the thread, so there is no thread id or name.
fn to_stack_trace(
    frames: Vec<ResolvedFrame>,
    backend_config: &BackendConfig,
    pid: u32,
) -> StackTrace {
    let frames = frames
        .into_iter()
        .map(|frame| StackFrame {
            name: frame.name,
            filename: frame.filename,
            line: frame.line.max(0) as u32,
        })
        .collect();
    StackTrace::new(backend_config, Some(pid), None, None, frames)
}

/// Starts a thread timer while preventing concurrent shutdown from dropping it.
fn thread_created(timer: &SharedTimer, tid: libc::pid_t) -> std::result::Result<(), String> {
    match timer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(timer) => timer
            .thread_created(tid)
            .map_err(|e| format!("failed to start the timer of thread {tid}: {e}")),
        None => Ok(()),
    }
}

fn thread_removed(timer: &SharedTimer, tid: libc::pid_t) {
    if let Some(timer) = timer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        timer.thread_removed(tid);
    }
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
                thread_removed(timer, previous);
            }
            if let Err(e) = thread_created(timer, tid) {
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
            thread_removed(timer, tid);
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
        thread_created(&timer, caller_tid).map_err(|e| {
            PyroscopeError::new(&format!(
                "Signal: failed to arm the configure caller's CPU timer: {e}"
            ))
        })?;
        for &tid in live.values() {
            if tid != caller_tid
                && let Err(e) = thread_created(&timer, tid)
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
mod tests {
    use super::*;
    use crate::{
        encode::{r#gen::google::Profile, pprof},
        utils::TimeRange,
    };
    use prost::Message;
    use std::time::{Duration, UNIX_EPOCH};

    #[cfg(not(miri))]
    #[test]
    fn shutdown_stops_timer_before_join_and_keeps_handler_until_thread_exits() {
        extern "C" fn noop(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {}

        let modes = [
            Mode::ITimer,
            #[cfg(target_os = "linux")]
            Mode::CTimer,
        ];
        for mode in modes {
            for panic_on_exit in [false, true] {
                let handler = SignalHandler::install(Timer::SIGNAL, noop).unwrap();
                // No sampling is needed to exercise ownership and shutdown.
                let timer = Timer::start(mode, Duration::from_secs(3600)).unwrap();
                let timer = Arc::new(Mutex::new(Some(timer)));
                let running = Arc::new(AtomicBool::new(true));
                let worker_timer = timer.clone();
                let worker_running = running.clone();
                let worker = std::thread::spawn(move || {
                    while worker_running.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                    assert!(worker_timer.lock().unwrap().is_none());
                    let error = SignalHandler::install(Timer::SIGNAL, noop).err().unwrap();
                    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
                    assert!(!panic_on_exit, "simulate sampler panic during shutdown");
                });
                let mut signal = Signal {
                    timer: timer.clone(),
                    handler: Some(handler),
                    buffer: Arc::new(Mutex::new(StackBuffer::default())),
                    backend_config: BackendConfig::default(),
                    sampler_thread: Some(worker),
                    running,
                };
                assert_eq!(signal.shutdown_thread().is_err(), panic_on_exit);
                assert!(timer.lock().unwrap().is_none());
                assert!(signal.sampler_thread.is_none());
                assert!(signal.handler.is_none());

                // Resources are reusable even after a sampler panic, and repeated shutdown
                // must not interfere with a replacement owner.
                let replacement_handler = SignalHandler::install(Timer::SIGNAL, noop).unwrap();
                let replacement_timer = Timer::start(mode, Duration::from_secs(3600)).unwrap();
                signal.shutdown_thread().unwrap();
                drop(signal);
                assert!(SignalHandler::install(Timer::SIGNAL, noop).is_err());
                assert!(Timer::start(mode, Duration::from_secs(3600)).is_err());
                drop(replacement_timer);
                drop(replacement_handler);
            }
        }
    }

    #[test]
    fn drop_reasons_are_single_frame_pprof_samples_with_exact_weights() {
        let config = BackendConfig {
            report_pid: true,
            ..BackendConfig::default()
        };
        let mut buffer = StackBuffer::default();
        buffer
            .record_with_count(
                StackTrace::new(
                    &config,
                    Some(123),
                    None,
                    None,
                    vec![
                        StackFrame {
                            name: "work".into(),
                            filename: "workload.py".into(),
                            line: 20,
                        },
                        StackFrame {
                            name: "main".into(),
                            filename: "workload.py".into(),
                            line: 10,
                        },
                    ],
                ),
                100,
            )
            .unwrap();
        record_dropped_stacks(&mut buffer, [("resolve_error", 0)], &config, 123).unwrap();
        assert_eq!(buffer.data.len(), 1, "{buffer:#?}");
        record_dropped_stacks(
            &mut buffer,
            [
                ("contentions", 1),
                ("buffer_dropped", 2),
                ("no_thread_state", 3),
                ("invalid_code_type", 4),
                ("read_errors", 5),
                ("truncated", 13),
                ("invalid", 8),
                ("resolve_error", 9),
            ],
            &config,
            123,
        )
        .unwrap();
        let range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();
        let bytes = pprof::encode(buffer.into(), 100, range).encode_to_vec();
        let profile = Profile::decode(bytes.as_slice()).unwrap();
        let mut actual: Vec<_> = profile
            .sample
            .iter()
            .map(|sample| {
                let frames: Vec<_> = sample
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
                    .collect();
                let labels: Vec<_> = sample
                    .label
                    .iter()
                    .map(|label| {
                        (
                            profile.string_table[label.key as usize].as_str(),
                            profile.string_table[label.str as usize].as_str(),
                        )
                    })
                    .collect();
                assert_eq!(labels, [("pid", "123")], "{profile:#?}");
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
            (vec![("[dropped: invalid]", "", 0)], vec![80_000_000]),
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
            1_450_000_000,
            "{profile:#?}"
        );
    }
}
