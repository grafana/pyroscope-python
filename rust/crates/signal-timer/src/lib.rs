//! CPU-time timers that deliver `SIGPROF`, modelled on async-profiler's `itimer` and `ctimer` modes.
//!
//! [`Mode::ITimer`] uses `setitimer(ITIMER_PROF)`: one timer for the whole process.
//! [`Mode::CTimer`] (Linux only) uses one `timer_create` timer per thread on that thread's CPU clock,
//! delivered to that thread.

#[cfg(target_os = "linux")]
mod ctimer;
mod itimer;

use std::ffi::c_void;
use std::io;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[cfg(target_os = "linux")]
use std::collections::hash_map::{Entry, HashMap};
#[cfg(target_os = "linux")]
use std::sync::Mutex;

pub use libc::{c_int, pid_t, siginfo_t};

pub type Handler = extern "C" fn(c_int, *mut siginfo_t, *mut c_void);

/// Ignores the signal on drop instead of restoring the previous action: a signal still pending
/// from a stopped timer would terminate the process under the default action.
pub struct SignalHandler {
    signal: c_int,
}

impl SignalHandler {
    pub fn install(signal: c_int, handler: Handler) -> io::Result<Self> {
        unsafe {
            let mut action: libc::sigaction = mem::zeroed();
            action.sa_sigaction = handler as usize;
            action.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(SignalHandler { signal })
        }
    }

    pub fn signal(&self) -> c_int {
        self.signal
    }
}

impl Drop for SignalHandler {
    fn drop(&mut self) {
        unsafe { libc::signal(self.signal, libc::SIG_IGN) };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ITimer,
    #[cfg(target_os = "linux")]
    CTimer,
}

/// Only one can exist at a time, whatever its mode. Stops all timers on drop.
pub struct Timer {
    backend: Backend,
    _active: Active,
}

enum Backend {
    ITimer(#[allow(dead_code)] itimer::ITimer),
    #[cfg(target_os = "linux")]
    CTimer {
        interval: Duration,
        threads: Mutex<HashMap<pid_t, ctimer::CTimer>>,
    },
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

struct Active;

impl Active {
    fn acquire() -> io::Result<Self> {
        ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .map(|_| Active)
            .map_err(|_| io::ErrorKind::AlreadyExists.into())
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

impl Timer {
    pub const SIGNAL: c_int = libc::SIGPROF;

    /// Fails with [`io::ErrorKind::AlreadyExists`] if another `Timer` is running.
    /// In [`Mode::CTimer`] no thread is sampled until it is passed to [`Timer::thread_created`].
    pub fn start(mode: Mode, interval: Duration) -> io::Result<Self> {
        if interval.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "interval must not be zero",
            ));
        }
        let active = Active::acquire()?;
        let backend = match mode {
            Mode::ITimer => Backend::ITimer(itimer::ITimer::start(interval)?),
            #[cfg(target_os = "linux")]
            Mode::CTimer => Backend::CTimer {
                interval,
                threads: Mutex::new(HashMap::new()),
            },
        };
        Ok(Timer {
            backend,
            _active: active,
        })
    }

    pub fn mode(&self) -> Mode {
        match self.backend {
            Backend::ITimer(_) => Mode::ITimer,
            #[cfg(target_os = "linux")]
            Backend::CTimer { .. } => Mode::CTimer,
        }
    }

    /// Starts sampling `tid` in [`Mode::CTimer`]. Does nothing if `tid` already has a timer,
    /// so call [`Timer::thread_removed`] before its tid is reused. Does nothing in [`Mode::ITimer`].
    pub fn thread_created(&self, tid: pid_t) -> io::Result<()> {
        match &self.backend {
            Backend::ITimer(_) => {
                let _ = tid;
                Ok(())
            }
            #[cfg(target_os = "linux")]
            Backend::CTimer { interval, threads } => {
                if let Entry::Vacant(e) = lock(threads).entry(tid) {
                    e.insert(ctimer::CTimer::start(tid, Self::SIGNAL, *interval)?);
                }
                Ok(())
            }
        }
    }

    /// Stops sampling `tid` in [`Mode::CTimer`]. Does nothing in [`Mode::ITimer`].
    pub fn thread_removed(&self, tid: pid_t) {
        match &self.backend {
            Backend::ITimer(_) => {
                let _ = tid;
            }
            #[cfg(target_os = "linux")]
            Backend::CTimer { threads, .. } => {
                lock(threads).remove(&tid);
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(target_os = "linux")]
pub fn gettid() -> pid_t {
    unsafe { libc::syscall(libc::SYS_gettid) as pid_t }
}

#[cfg(all(test, not(miri)))]
mod tests;
