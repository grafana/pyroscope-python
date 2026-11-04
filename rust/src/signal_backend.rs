use crate::{
    backend::{BackendConfig, StackBuffer, StackFrame, StackTrace},
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
    buffer: Arc<Mutex<StackBuffer>>,
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

        let buffer = Arc::new(Mutex::new(StackBuffer::default()));
        let running = Arc::new(AtomicBool::new(true));
        let sampler_thread = {
            let buffer = buffer.clone();
            let running = running.clone();
            std::thread::spawn(move || {
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
