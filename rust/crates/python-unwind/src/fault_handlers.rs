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
            kindasafe_init::init().map_err(InitError::Install)?;
            let mut addresses = [0; 2];
            for (address, signal) in addresses.iter_mut().zip(SIGNALS) {
                *address = action(signal).ok_or(InitError::Inspect)?.sa_sigaction;
            }
            ENABLED.store(true, Ordering::Release);
            Ok(Handlers { addresses })
        })
        .as_ref()
        .map_err(Clone::clone)?;
    if recovery_active() {
        Ok(())
    } else {
        Err(InitError::Replaced)
    }
}

/// Checks both dispositions without allocating or locking. Call before fault-prone reads,
/// including reads outside the signal handler. Once takeover is observed, reading stays disabled.
/// As with any disposition check, a concurrent foreign sigaction can race the following read.
pub fn recovery_active() -> bool {
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
