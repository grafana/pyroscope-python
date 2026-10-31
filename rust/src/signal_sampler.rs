//! Samples the current process by unwinding python stacks inside a `SIGPROF` handler.
//!
//! The handler lives in `unwind::handler`. It records code object addresses into a
//! ring buffer. A normal thread drains it and resolves names, tolerating code objects that were
//! freed in the meantime. A tick interrupts one running thread, so only threads on the CPU are
//! sampled.

use std::sync::{Arc, Mutex};
#[cfg(Py_3_11)]
use std::time::Duration;

#[cfg(Py_3_11)]
use ringbuffer::PopError;
use timer::Timer;
#[cfg(Py_3_11)]
use timer::{Mode, SignalHandler};
#[cfg(Py_3_11)]
use unwind::handler;
#[cfg(Py_3_11)]
use unwind::{Frame, MAX_FRAMES};

#[cfg(Py_3_11)]
mod resolve;
#[cfg(Py_3_11)]
use pyroscope_ringbuffer as ringbuffer;
use pyroscope_signal_timer as timer;
#[cfg(Py_3_11)]
#[allow(dead_code)]
pub mod unwind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lines {
    /// Every line is 0.
    None,
    /// The first line of the function.
    First,
    /// The line of the instruction the frame executes.
    LastInstruction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFrame {
    pub name: String,
    pub filename: String,
    pub line: u32,
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
#[cfg_attr(not(Py_3_11), allow(dead_code))]
pub struct Config {
    pub python_version: (u8, u8),
    pub sampling_rate: u32,
    pub signal_timer: SignalTimer,
    pub lines: Lines,
}

pub type Error = String;

#[cfg(Py_3_11)]
fn check_version(version: (u8, u8)) -> Result<(), Error> {
    let (major, minor) = version;
    if !matches!(version, (3, 11..=14)) {
        return Err(format!(
            "unwinding in the signal handler supports only Python 3.11 to 3.14, found {major}.{minor}"
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
#[cfg(Py_3_11)]
pub struct SignalHandlerSession {
    timer: SharedTimer,
    // Dropped after the timer, see the `Drop` impl.
    _handler: SignalHandler,
    buf: Vec<Frame>,
    resolver: resolve::Resolver,
}

#[cfg(Py_3_11)]
impl SignalHandlerSession {
    pub fn start(config: &Config) -> Result<SignalHandlerSession, Error> {
        check_version(config.python_version)?;
        unwind::init().map_err(|e| format!("failed to install memory fault handlers: {:?}", e))?;
        let types = resolve::Types {
            unicode: (&raw mut pyo3::ffi::PyUnicode_Type) as usize,
            bytes: (&raw mut pyo3::ffi::PyBytes_Type) as usize,
        };
        handler::configure();

        // Discard what a previous session left behind.
        let mut buf = vec![Frame::default(); MAX_FRAMES];
        if let Some(mut consumer) = handler::consumer() {
            while consumer.pop_slice(&mut buf).is_ok() {}
        }

        let signal_handler = SignalHandler::install(Timer::SIGNAL, handler::handler)
            .map_err(|e| format!("failed to install the signal handler: {e}"))?;
        let interval = Duration::from_secs_f64(1.0 / config.sampling_rate.max(1) as f64);
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
            resolver: resolve::Resolver::new(types, config.lines),
        })
    }

    pub fn timer(&self) -> SharedTimer {
        self.timer.clone()
    }

    /// Resolves everything the handler recorded since the last call. Each stack is innermost
    /// frame first.
    pub fn drain(&mut self) -> (Vec<Vec<ResolvedFrame>>, Vec<Error>) {
        let mut traces = Vec::new();
        let mut errors = Vec::new();
        let Some(mut consumer) = handler::consumer() else {
            return (traces, errors);
        };
        loop {
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

#[cfg(Py_3_11)]
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

// No session can exist on a build without signal-unwinder layouts.
#[cfg(not(Py_3_11))]
pub enum SignalHandlerSession {}

#[cfg(not(Py_3_11))]
impl Drop for SignalHandlerSession {
    fn drop(&mut self) {
        match *self {}
    }
}

#[cfg(not(Py_3_11))]
impl SignalHandlerSession {
    pub fn start(config: &Config) -> Result<Self, Error> {
        let (major, minor) = config.python_version;
        Err(format!(
            "unwinding in the signal handler supports only Python 3.11 to 3.14, found {major}.{minor}"
        ))
    }

    pub fn timer(&self) -> SharedTimer {
        match *self {}
    }

    pub fn drain(&mut self) -> (Vec<Vec<ResolvedFrame>>, Vec<Error>) {
        match *self {}
    }
}

#[cfg(all(test, not(Py_3_11)))]
mod tests {
    use super::*;

    #[test]
    fn python_310_rejects_both_signal_timers() {
        for signal_timer in [SignalTimer::ITimer, SignalTimer::CTimer] {
            let config = Config {
                python_version: (3, 10),
                sampling_rate: 100,
                signal_timer,
                lines: Lines::LastInstruction,
            };
            assert_eq!(
                SignalHandlerSession::start(&config).err().unwrap(),
                "unwinding in the signal handler supports only Python 3.11 to 3.14, found 3.10"
            );
        }
    }
}
