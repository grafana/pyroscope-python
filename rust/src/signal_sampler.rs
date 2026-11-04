//! Samples the current process by unwinding python stacks inside a `SIGPROF` handler.
//!
//! The C signal callback delegates to `unwind::handler`, which records code object addresses into a
//! ring buffer. A normal thread drains it and resolves names, tolerating code objects that were
//! freed in the meantime. A tick interrupts one running thread, so only threads on the CPU are
//! sampled.

use ringbuffer::PopError;
use timer::{SignalHandler, Timer};
use unwind::{Frame, MAX_FRAMES};

mod resolve;
pub use py_spy::{Frame as ResolvedFrame, config::LineNo as Lines};
use pyroscope_python_unwind as unwind;
use pyroscope_ringbuffer as ringbuffer;
use pyroscope_signal_timer as timer;

extern "C" fn signal_handler(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    unwind::handler();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalTimer {
    /// One `setitimer(ITIMER_PROF)` timer for the process: each tick interrupts an arbitrary
    /// running thread.
    ITimer,
    /// Linux only. One timer per thread on its CPU clock: every thread gets ticks in proportion to
    /// the CPU time it uses. A thread is sampled only after its OS thread id is registered
    /// by the signal backend's thread hooks.
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

#[cfg(test)]
pub(super) static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Drains and resolves signal samples. The caller owns the handler and timer lifetimes.
pub struct SignalHandlerSession {
    buf: Vec<Frame>,
    resolver: resolve::Resolver,
}

impl SignalHandlerSession {
    /// Initializes sampling state and installs the handler. The caller must keep the
    /// returned guard alive until its timers have stopped and its resolver has exited.
    pub fn start(config: &Config) -> Result<(Self, SignalHandler), Error> {
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
        // Discard drop counts left by the previous session before arming the timer.
        unwind::METRICS.take_drop_counts();
        Ok((Self { buf, resolver }, signal_handler))
    }

    /// Resolves a bounded batch of recorded stacks, each innermost frame first.
    /// Returning even while producers are busy lets the sampling thread upload and stop.
    pub fn drain(&mut self) -> Vec<Vec<ResolvedFrame>> {
        let mut traces = Vec::new();
        let Some(mut consumer) = unwind::consumer() else {
            return traces;
        };
        for _ in 0..MAX_DRAIN_STACKS {
            let n = match consumer.pop_slice(&mut self.buf) {
                Ok(n) => n,
                Err(PopError::Empty) => break,
                Err(PopError::TooSmall { .. }) => unreachable!("buffer fits MAX_FRAMES"),
            };
            if let Ok(trace) = self.resolver.resolve_stack(&self.buf[..n]) {
                traces.push(trace);
            }
        }
        traces
    }
}
