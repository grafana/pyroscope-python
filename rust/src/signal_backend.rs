use crate::{
    backend::{BackendConfig, StackBuffer, StackFrame, StackTrace, ThreadTagsSet},
    error::{PyroscopeError, Result},
    pyspy_backend::Reporter,
    signal_sampler::{self, ResolvedFrame, SharedTimer, SignalHandlerSession, SignalTimer},
};
use pyo3::prelude::*;
use pyo3::wrap_pyfunction;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
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
    buffer: Arc<Mutex<StackBuffer>>,
    sampler_thread: Option<JoinHandle<()>>,
    running: Arc<AtomicBool>,
}

impl Signal {
    pub fn new(
        config: Config,
        backend_config: BackendConfig,
        ruleset: ThreadTagsSet,
    ) -> Result<Self> {
        let mut session = SignalHandlerSession::start(&config.sampler)
            .map_err(|e| PyroscopeError::new(&format!("Signal: {e}")))?;
        if config.sampler.signal_timer == SignalTimer::CTimer {
            threads::attach(session.timer())?;
        }

        let buffer = Arc::new(Mutex::new(StackBuffer::default()));
        let running = Arc::new(AtomicBool::new(true));
        let sampler_thread = {
            let buffer = buffer.clone();
            let running = running.clone();
            std::thread::spawn(move || {
                let mut reported_errors = 0;
                while running.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(10)); //todo do not wake up on every 10ms, notify from producer when buf is half full
                    let (traces, errors) = session.drain();
                    for e in errors {
                        if reported_errors < 239 {
                            reported_errors += 1;
                            log::debug!(target: LOG_TAG, "dropped a stack: {e}");
                        }
                    }
                    let Ok(mut buffer) = buffer.lock() else {
                        break;
                    };
                    for frames in traces {
                        let trace = to_stack_trace(frames, &backend_config);
                        let _ = buffer.record(trace.add_tag_rules(&ruleset));
                    }
                }
                if config.sampler.signal_timer == SignalTimer::CTimer {
                    threads::detach();
                }
                drop(session);
            })
        };

        Ok(Signal {
            buffer,
            sampler_thread: Some(sampler_thread),
            running,
        })
    }

    pub fn reporter(&self) -> Reporter {
        Reporter::new(self.buffer.clone())
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

/// The handler does not record the thread, so there is no thread id or name.
fn to_stack_trace(frames: Vec<ResolvedFrame>, backend_config: &BackendConfig) -> StackTrace {
    let frames = frames
        .into_iter()
        .map(|frame| StackFrame {
            name: frame.name,
            filename: frame.filename,
            line: frame.line,
        })
        .collect();
    StackTrace::new(backend_config, Some(std::process::id()), None, None, frames)
}

/// Feeds the OS thread ids of python threads to a `CTimer` session. The hooks stay installed
/// for the life of the process and keep the live threads in `LIVE`, so a session that starts
/// later still learns about the threads that started before it.
pub mod threads {
    use super::*;

    static INSTALLED: OnceLock<()> = OnceLock::new();
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
            return Ok(());
        }
        py.import("pyroscope")?
            .getattr("_install_stack_threads")?
            .call1((
                py.import("threading")?,
                wrap_pyfunction!(register_thread, py)?,
                wrap_pyfunction!(unregister_thread, py)?,
            ))?;
        let _ = INSTALLED.set(());
        Ok(())
    }

    pub(super) fn attach(timer: SharedTimer) -> Result<()> {
        if INSTALLED.get().is_none() {
            return Err(PyroscopeError::new(
                "Signal: the thread hooks of the CTimer are not installed",
            ));
        }
        let mut slot = lock(&TIMER);
        for &tid in lock(&LIVE).get_or_insert_default().values() {
            if let Err(e) = signal_sampler::thread_created(&timer, tid) {
                log::warn!(target: LOG_TAG, "{e}");
            }
        }
        *slot = Some(timer);
        Ok(())
    }

    pub(super) fn detach() {
        lock(&TIMER).take();
    }
}
