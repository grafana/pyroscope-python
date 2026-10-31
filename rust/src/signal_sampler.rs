//! Samples the current process by unwinding python stacks inside a `SIGPROF` handler.
//!
//! The handler lives in [`unwind::handler`]. It records code object addresses into a
//! ring buffer. A normal thread drains it and resolves names, tolerating code objects that were
//! freed in the meantime. A tick interrupts one running thread, so only threads on the CPU are
//! sampled.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ringbuffer::PopError;
use timer::{Mode, SignalHandler, Timer};
use unwind::handler;
use unwind::{Frame, ThreadStateGetter};

mod resolve;
// Copied as is from pyroscope-py-spy, where its tests use the rest.
#[allow(dead_code)]
pub mod ringbuffer;
#[allow(dead_code)]
pub mod timer;
#[allow(dead_code)]
pub mod unwind;

pub use resolve::{Lines, ResolvedFrame};

const MAX_FRAMES: usize = 4096; //todo wtf

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

fn lookup(name: &std::ffi::CStr) -> Result<usize, Error> {
    let addr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) } as usize;
    if addr == 0 {
        return Err(format!("symbol {:?} not found", name));
    }
    Ok(addr)
}

fn lookup_thread_state_getter() -> Result<ThreadStateGetter, Error> {
    // TODO: PyGILState_GetThisThreadState is not safe on 3.15; find another way to get the tstate.
    let addr = lookup(c"PyGILState_GetThisThreadState")?;
    // SAFETY: the symbol has this signature.
    Ok(unsafe { std::mem::transmute::<usize, ThreadStateGetter>(addr) })
}

fn layout_for(version: (u8, u8)) -> Result<&'static resolve::Layout, Error> {
    if version != unwind::PY_VERSION {
        let (major, minor) = version;
        let (built_major, built_minor) = unwind::PY_VERSION;
        return Err(format!(
            "running on Python {major}.{minor}, but built against Python {built_major}.{built_minor}"
        ));
    }
    Ok(match version {
        (3, 11) => &resolve::V3_11,
        (3, 12) => &resolve::V3_12,
        (3, 13) => &resolve::V3_13,
        (3, 14) => &resolve::V3_14,
        (major, minor) => {
            return Err(format!(
                "unwinding in the signal handler supports only Python 3.11 to 3.14, found {major}.{minor}"
            ));
        }
    })
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
        let layout = layout_for(config.python_version)?;
        unwind::init().map_err(|e| format!("failed to install memory fault handlers: {:?}", e))?;
        let code_type = lookup(c"PyCode_Type")?;
        let types = resolve::Types {
            code: code_type,
            unicode: lookup(c"PyUnicode_Type")?,
            bytes: lookup(c"PyBytes_Type")?,
        };
        handler::configure(code_type, lookup_thread_state_getter()?, MAX_FRAMES);

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
            resolver: resolve::Resolver::new(layout, types, config.lines),
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

impl Drop for SignalHandlerSession {
    /// Stops the timers before the handler is removed. Holding the lock keeps a concurrent hook
    /// from using a dead timer.
    fn drop(&mut self) {
        *self.timer.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Starts sampling `tid` if the session runs in [`Mode::CTimer`]. Does nothing otherwise.
pub fn thread_created(timer: &SharedTimer, tid: timer::pid_t) -> Result<(), Error> {
    match timer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        Some(timer) => timer
            .thread_created(tid)
            .map_err(|e| format!("failed to start the timer of thread {tid}: {e}")),
        None => Ok(()),
    }
}

/// Stops sampling `tid`. Does nothing if the session ended or runs in [`Mode::ITimer`].
pub fn thread_removed(timer: &SharedTimer, tid: timer::pid_t) {
    if let Some(timer) = timer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        timer.thread_removed(tid);
    }
}

#[cfg(test)]
mod tests;
