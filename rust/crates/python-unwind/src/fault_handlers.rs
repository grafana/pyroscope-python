//! Tracks the recovery handlers installed by kindasafe. A foreign replacement is authoritative:
//! stop reading memory and refuse later starts instead of reclaiming its signals.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone)]
pub enum InitError {
    Install(kindasafe_init::InitError),
    Inspect,
    Replaced,
}

struct Handlers {
    addresses: [usize; 2],
}

const SIGNALS: [libc::c_int; 2] = [libc::SIGSEGV, libc::SIGBUS];
static HANDLERS: OnceLock<Result<Handlers, InitError>> = OnceLock::new();
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init() -> Result<(), InitError> {
    HANDLERS
        .get_or_init(|| {
            // SAFETY: SDK configuration serializes our installation. Applications must
            // coordinate foreign disposition changes as documented by this crate; callbacks
            // we chain to must remain loaded. The checks below detect completed takeovers,
            // but cannot make uncoordinated foreign sigaction calls safe.
            unsafe { kindasafe_init::init() }.map_err(InitError::Install)?;
            let mut addresses = [0; 2];
            for (address, signal) in addresses.iter_mut().zip(SIGNALS) {
                *address = action(signal).ok_or(InitError::Inspect)?.sa_sigaction;
            }
            ENABLED.store(true, Ordering::Release);
            Ok(Handlers { addresses })
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if handlers_active() {
        Ok(())
    } else {
        Err(InitError::Replaced)
    }
}

/// Checks both dispositions and the calling thread's active signal mask without allocating or
/// locking. Once takeover is observed, reading stays disabled; a blocked mask only skips this
/// call and does not disable other threads or later samples.
/// As with any disposition check, a concurrent foreign sigaction can race the following read.
pub fn recovery_active() -> bool {
    // A blocked signal is temporary and thread-local. It must not latch ENABLED=false.
    handlers_active() && signals_unblocked()
}

// kindasafe performs the load directly, so check the calling thread's mask here
// and before each read. Returning nested handlers restore the checked mask.
pub(super) fn signals_unblocked() -> bool {
    // SAFETY: a null set only queries the active mask. Both calls are async-signal-safe,
    // and SIGNALS contains valid signal numbers.
    unsafe {
        let mut mask = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask) == 0
            && SIGNALS
                .into_iter()
                .all(|signal| libc::sigismember(&mask, signal) == 0)
    }
}

fn handlers_active() -> bool {
    if !ENABLED.load(Ordering::Acquire) {
        return false;
    }
    let Some(Ok(handlers)) = HANDLERS.get() else {
        return false;
    };
    for (signal, address) in SIGNALS.into_iter().zip(handlers.addresses) {
        let owned = action(signal).is_some_and(|current| {
            current.sa_sigaction == address
                && current.sa_flags & libc::SA_SIGINFO != 0
                && current.sa_flags & libc::SA_RESETHAND == 0
        });
        if !owned {
            ENABLED.store(false, Ordering::Release);
            return false;
        }
    }
    true
}

fn action(signal: libc::c_int) -> Option<libc::sigaction> {
    // SAFETY: sigaction is async-signal-safe; a null new action only queries the disposition.
    unsafe {
        let mut current = std::mem::zeroed();
        (libc::sigaction(signal, std::ptr::null(), &mut current) == 0).then_some(current)
    }
}
