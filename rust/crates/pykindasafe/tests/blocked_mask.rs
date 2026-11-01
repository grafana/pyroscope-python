//! Only valid immutable source bytes are used: blocked cases must decline before
//! assembly, without deliberately causing an inaccessible-address fault.
use pykindasafe::{ReadMemError, RecoveryError};
use std::sync::atomic::{AtomicI32, Ordering};

static SOURCE: [u8; 8] = *b"sample\0\0";
static EXPECTED_BLOCKED: AtomicI32 = AtomicI32::new(0);
static CALLBACK_RESULT: AtomicI32 = AtomicI32::new(0);

fn errno_location() -> *mut libc::c_int {
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::__errno_location() }
    }
    #[cfg(target_os = "macos")]
    {
        unsafe { libc::__error() }
    }
}

fn declined(signal: libc::c_int) -> bool {
    let error = RecoveryError::SignalBlocked { signal };
    let mut destination = [0xa5; 8];
    unsafe {
        *errno_location() = libc::EDOM;
        pykindasafe::check_recovery() == Err(error)
            && pykindasafe::u64(SOURCE.as_ptr() as u64)
                == Err(ReadMemError::RecoveryUnavailable(error))
            && pykindasafe::slice(&mut destination, SOURCE.as_ptr() as u64)
                == Err(ReadMemError::RecoveryUnavailable(error))
            && pykindasafe::str(&mut destination, SOURCE.as_ptr() as u64)
                == Err(ReadMemError::RecoveryUnavailable(error))
            && destination == [0xa5; 8]
            && *errno_location() == libc::EDOM
    }
}

unsafe extern "C" fn leaf(_: libc::c_int) {
    CALLBACK_RESULT.store(
        if declined(EXPECTED_BLOCKED.load(Ordering::Relaxed)) {
            1
        } else {
            -1
        },
        Ordering::Relaxed,
    );
}

unsafe extern "C" fn outer(_: libc::c_int) {
    if unsafe { libc::raise(libc::SIGUSR1) } != 0 {
        CALLBACK_RESULT.store(-2, Ordering::Relaxed);
    }
}

#[test]
fn mask_contexts() {
    for case in ["uninitialized", "thread", "handler_mask", "nested_handler"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mask_child", "--nocapture"])
            .env("PYKINDASAFE_MASK_CASE", case)
            .output()
            .unwrap();
        assert!(result.status.success(), "{case}: {result:?}");
    }
}

#[test]
fn mask_child() {
    let Ok(case) = std::env::var("PYKINDASAFE_MASK_CASE") else {
        return;
    };
    if case == "uninitialized" {
        let error = ReadMemError::RecoveryUnavailable(RecoveryError::NotInitialized);
        assert_eq!(
            unsafe { pykindasafe::u64(SOURCE.as_ptr() as u64) },
            Err(error)
        );
        assert_eq!(
            unsafe { pykindasafe::slice(&mut [0; 8], SOURCE.as_ptr() as u64) },
            Err(error)
        );
        assert_eq!(
            unsafe { pykindasafe::sanity_check() },
            Err(pykindasafe::InitError::SanityCheckFailed)
        );
        return;
    }
    unsafe { pykindasafe::init() }.unwrap();
    pykindasafe::check_recovery().unwrap();
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        if case == "thread" {
            std::thread::spawn(move || unsafe {
                let mut blocked: libc::sigset_t = std::mem::zeroed();
                let mut old: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut blocked);
                libc::sigaddset(&mut blocked, signal);
                assert_eq!(
                    libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut old),
                    0
                );
                assert!(declined(signal));
                // A denied read must not silently unblock the signal.
                assert!(declined(signal));
                assert_eq!(
                    pykindasafe::sanity_check(),
                    Err(pykindasafe::InitError::SanityCheckFailed)
                );
                assert_eq!(
                    libc::pthread_sigmask(libc::SIG_SETMASK, &old, std::ptr::null_mut()),
                    0
                );
                pykindasafe::check_recovery().unwrap();
                assert!(pykindasafe::u64(SOURCE.as_ptr() as u64).is_ok());
            })
            .join()
            .unwrap();
        } else {
            unsafe {
                EXPECTED_BLOCKED.store(signal, Ordering::Relaxed);
                CALLBACK_RESULT.store(0, Ordering::Relaxed);
                let mut inner: libc::sigaction = std::mem::zeroed();
                let mut enclosing: libc::sigaction = std::mem::zeroed();
                libc::sigemptyset(&mut inner.sa_mask);
                libc::sigemptyset(&mut enclosing.sa_mask);
                inner.sa_sigaction = leaf as *const () as usize;
                enclosing.sa_sigaction = outer as *const () as usize;
                if case == "handler_mask" {
                    libc::sigaddset(&mut inner.sa_mask, signal);
                } else {
                    libc::sigaddset(&mut enclosing.sa_mask, signal);
                }
                assert_eq!(
                    libc::sigaction(libc::SIGUSR1, &inner, std::ptr::null_mut()),
                    0
                );
                assert_eq!(
                    libc::sigaction(libc::SIGUSR2, &enclosing, std::ptr::null_mut()),
                    0
                );
                assert_eq!(
                    libc::raise(if case == "handler_mask" {
                        libc::SIGUSR1
                    } else {
                        libc::SIGUSR2
                    }),
                    0
                );
                assert_eq!(CALLBACK_RESULT.load(Ordering::Relaxed), 1);
            }
            pykindasafe::check_recovery().unwrap();
        }
    }
}
