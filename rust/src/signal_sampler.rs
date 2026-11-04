//! Samples the current process by unwinding python stacks inside a `SIGPROF` handler.
//!
//! The C signal callback delegates to `unwind::handler`, which records code object addresses into a
//! ring buffer. A normal thread drains it and resolves names, tolerating code objects that were
//! freed in the meantime. A tick interrupts one running thread, so only threads on the CPU are
//! sampled.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ringbuffer::PopError;
use timer::Timer;
use timer::{Mode, SignalHandler};
use unwind::{Frame, MAX_FRAMES};

mod resolve;
pub use py_spy::{Frame as ResolvedFrame, config::LineNo as Lines};
use pyroscope_python_unwind as unwind;
use pyroscope_ringbuffer as ringbuffer;
use pyroscope_signal_timer as timer;
pub use resolve::Error as ResolveError;

extern "C" fn signal_handler(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    unwind::handler();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalTimer {
    /// One `setitimer(ITIMER_PROF)` timer for the process: each tick interrupts an arbitrary
    /// running thread.
    ITimer,
    /// Linux only. One timer per thread on its CPU clock: every thread gets ticks in proportion to
    /// the CPU time it uses. A thread is sampled only after [`thread_created`] was called
    /// for its OS thread id.
    CTimer,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    pub python_version: (u8, u8),
    pub sampling_rate: u32,
    pub signal_timer: SignalTimer,
    pub lines: Lines,
}

pub type Error = String;

const MAX_DRAIN_STACKS: usize = 64;

fn check_version(version: (u8, u8)) -> Result<(), Error> {
    let (major, minor) = version;
    if !matches!(version, (3, 10..=14)) {
        return Err(format!(
            "unwinding in the signal handler supports only Python 3.10 to 3.14, found {major}.{minor}"
        ));
    }
    if version != unwind::PY_VERSION {
        let (built_major, built_minor) = unwind::PY_VERSION;
        return Err(format!(
            "running on Python {major}.{minor}, but built against Python {built_major}.{built_minor}"
        ));
    }
    Ok(())
}

/// The timer of a running [`SignalHandlerSession`], shared with the callers of
/// [`thread_created`] and [`thread_removed`]. `None` once the session ended.
pub type SharedTimer = Arc<Mutex<Option<Timer>>>;

/// Running signal sampling. Only one can exist at a time.
pub struct SignalHandlerSession {
    timer: SharedTimer,
    // Dropped after the timer, see the `Drop` impl.
    _handler: SignalHandler,
    buf: Vec<Frame>,
    resolver: resolve::Resolver,
}

impl SignalHandlerSession {
    pub fn start(config: &Config) -> Result<SignalHandlerSession, Error> {
        check_version(config.python_version)?;
        if config.sampling_rate == 0 {
            return Err("the sampling rate must be greater than zero".to_string());
        }
        // SAFETY: kindasafe serializes and caches initialization, including failures.
        // SDK users must coordinate other SIGSEGV/SIGBUS users during installation,
        // preserve the installed handlers across profiler restarts, and keep chained
        // callbacks compatible and loaded, as documented in README.md and the unwinder.
        unsafe { kindasafe_init::init() }
            .map_err(|error| format!("failed to install memory fault handlers: {error:?}"))?;
        let types = py_spy::python_process_info::Types {
            code: (&raw mut pyo3::ffi::PyCode_Type) as usize,
            string: (&raw mut pyo3::ffi::PyUnicode_Type) as usize,
            dict: (&raw mut pyo3::ffi::PyDict_Type) as usize,
        };

        let resolver = resolve::Resolver::new(types, config.lines);

        // Discard what a previous session left behind.
        let mut buf = vec![Frame::default(); MAX_FRAMES];
        if let Some(mut consumer) = unwind::consumer() {
            while consumer.pop_slice(&mut buf).is_ok() {}
        }

        let signal_handler = SignalHandler::install(Timer::SIGNAL, signal_handler)
            .map_err(|e| format!("failed to install the signal handler: {e}"))?;
        let interval = Duration::from_secs_f64(1.0 / config.sampling_rate as f64);
        let mode = match config.signal_timer {
            SignalTimer::ITimer => Mode::ITimer,
            #[cfg(target_os = "linux")]
            SignalTimer::CTimer => Mode::CTimer,
            #[cfg(not(target_os = "linux"))]
            SignalTimer::CTimer => return Err("CTimer works only on Linux".to_string()),
        };
        let timer =
            Timer::start(mode, interval).map_err(|e| format!("failed to start the timer: {e}"))?;
        Ok(SignalHandlerSession {
            timer: Arc::new(Mutex::new(Some(timer))),
            _handler: signal_handler,
            buf,
            resolver,
        })
    }

    pub fn timer(&self) -> SharedTimer {
        self.timer.clone()
    }

    /// Resolves a bounded batch of recorded stacks, each innermost frame first.
    /// Returning even while producers are busy lets the sampling thread upload and stop.
    pub fn drain(&mut self) -> (Vec<Vec<ResolvedFrame>>, Vec<ResolveError>) {
        let mut traces = Vec::new();
        let mut errors = Vec::new();
        let Some(mut consumer) = unwind::consumer() else {
            return (traces, errors);
        };
        for _ in 0..MAX_DRAIN_STACKS {
            let n = match consumer.pop_slice(&mut self.buf) {
                Ok(n) => n,
                Err(PopError::Empty) => break,
                Err(PopError::TooSmall { .. }) => unreachable!("buffer fits MAX_FRAMES"),
            };
            match self.resolver.resolve_stack(&self.buf[..n]) {
                Ok(trace) => traces.push(trace),
                Err(e) => errors.push(e),
            }
        }
        (traces, errors)
    }
}

impl Drop for SignalHandlerSession {
    /// Stops the timers before the handler is removed. Holding the lock keeps a concurrent hook
    /// from using a dead timer.
    fn drop(&mut self) {
        *self.timer.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Starts sampling `tid` if the session runs in [`timer::Mode::CTimer`]. Does nothing otherwise.
pub fn thread_created(timer: &SharedTimer, tid: timer::pid_t) -> Result<(), Error> {
    match timer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(timer) => timer
            .thread_created(tid)
            .map_err(|e| format!("failed to start the timer of thread {tid}: {e}")),
        None => Ok(()),
    }
}

/// Stops sampling `tid`. Does nothing if the session ended or runs in [`timer::Mode::ITimer`].
pub fn thread_removed(timer: &SharedTimer, tid: timer::pid_t) {
    if let Some(timer) = timer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        timer.thread_removed(tid);
    }
}
