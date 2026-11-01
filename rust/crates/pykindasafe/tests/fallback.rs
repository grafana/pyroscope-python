//! Each case gets a fresh process, including expected default-signal death.
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);
static SELF_BLOCKED: AtomicI32 = AtomicI32::new(1);

unsafe extern "C" fn one_arg(sig: libc::c_int) {
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        if sig != libc::SIGBUS
            || libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask) != 0
            || libc::sigismember(&mask, libc::SIGUSR1) != 1
            || libc::sigismember(&mask, sig) != SELF_BLOCKED.load(Ordering::Relaxed)
        {
            libc::_exit(90);
        }
    }
    CALLS.fetch_add(1, Ordering::Relaxed);
}

unsafe extern "C" fn three_args(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void,
) {
    unsafe {
        if info.is_null() || data.is_null() || (*info).si_signo != sig {
            libc::_exit(91);
        }
        one_arg(sig);
    }
}

#[test]
fn dispositions() {
    for (case, expected_signal) in [
        ("ignore", None),
        ("one", None),
        ("three", None),
        ("nodefer", None),
        ("restart", None),
        ("parallel", None),
        ("default_bus", Some(libc::SIGBUS)),
        ("default_segv", Some(libc::SIGSEGV)),
        ("reset", Some(libc::SIGBUS)),
    ] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fallback_child", "--nocapture"])
            .env("PYKINDASAFE_FALLBACK_CASE", case)
            .output()
            .unwrap();
        assert_eq!(
            result.status.signal(),
            expected_signal,
            "{case}: {result:?}"
        );
        if expected_signal.is_none() {
            assert!(result.status.success(), "{case}: {result:?}");
        }
    }
}

#[test]
fn fallback_child() {
    let Ok(case) = std::env::var("PYKINDASAFE_FALLBACK_CASE") else {
        return;
    };
    unsafe {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_CORE, &limit), 0);
        let signal = if case == "default_segv" {
            libc::SIGSEGV
        } else {
            libc::SIGBUS
        };
        let mut action: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaddset(&mut action.sa_mask, libc::SIGUSR1);
        action.sa_sigaction = match case.as_str() {
            "ignore" => libc::SIG_IGN,
            "default_bus" | "default_segv" => libc::SIG_DFL,
            "three" => three_args as *const () as usize,
            _ => one_arg as *const () as usize,
        };
        action.sa_flags = match case.as_str() {
            "three" => libc::SA_SIGINFO,
            "nodefer" => {
                SELF_BLOCKED.store(0, Ordering::Relaxed);
                libc::SA_NODEFER
            }
            "restart" => libc::SA_RESTART | libc::SA_ONSTACK,
            "reset" => libc::SA_RESETHAND,
            _ => 0,
        };
        assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
        pykindasafe::init().unwrap();
        let mut installed: libc::sigaction = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut installed), 0);
        assert_eq!(
            installed.sa_flags & (libc::SA_RESTART | libc::SA_NODEFER | libc::SA_ONSTACK),
            action.sa_flags & (libc::SA_RESTART | libc::SA_NODEFER | libc::SA_ONSTACK)
        );
        assert_eq!(installed.sa_flags & libc::SA_RESETHAND, 0);
        assert_eq!(libc::sigismember(&installed.sa_mask, libc::SIGUSR1), 1);
        // Recovery must survive repeated reads, including with a one-shot prior
        // disposition. Uses the existing upstream bounded sanity-check mapping.
        pykindasafe::sanity_check().unwrap();
        pykindasafe::sanity_check().unwrap();
        if case == "parallel" {
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    scope.spawn(move || {
                        for _ in 0..16 {
                            assert_eq!(libc::raise(signal), 0);
                        }
                    });
                }
            });
            assert_eq!(CALLS.load(Ordering::Relaxed), 64);
            return;
        }
        assert_eq!(libc::raise(signal), 0);
        if case == "reset" {
            assert_eq!(CALLS.load(Ordering::Relaxed), 1);
            pykindasafe::sanity_check().unwrap();
        }
        assert_eq!(libc::raise(signal), 0);
        assert_eq!(
            CALLS.load(Ordering::Relaxed),
            if case == "ignore" { 0 } else { 2 }
        );
    }
}
