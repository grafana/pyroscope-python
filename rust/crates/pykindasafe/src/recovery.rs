use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};

#[derive(Debug, PartialEq, Clone)]
pub enum InitError {
    InstallSignalHandlersFailed,
    SanityCheckFailed,
}

type SignalHandler = unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void);

// Every cfg-selected platform callback must satisfy the OS C ABI at compile time.
const _: SignalHandler = crash_handler;

// These pointer atomics are lock-free on all supported 64-bit targets. Each
// snapshot is initialized in ordinary thread context and then immutable forever.
// We intentionally retain it even on partial installation failure: a delivery
// may already hold its address. Neither handlers nor initialization reclaim it.
struct PublishedAction(AtomicPtr<libc::sigaction>);

impl PublishedAction {
    const fn new() -> Self {
        Self(AtomicPtr::new(std::ptr::null_mut()))
    }

    fn publish(&self, action: libc::sigaction) {
        // Called once per slot under INIT_LOCK, never from a signal handler.
        assert!(self.0.load(Ordering::Relaxed).is_null());
        self.0
            .store(Box::into_raw(Box::new(action)), Ordering::Release);
    }

    fn get(&self) -> Option<&libc::sigaction> {
        // SAFETY: Release publication precedes handler installation; snapshots
        // are immutable and never freed, including after an install failure.
        unsafe { self.0.load(Ordering::Acquire).as_ref() }
    }
}

static FALLBACK_SIGSEGV: PublishedAction = PublishedAction::new();
static FALLBACK_SIGBUS: PublishedAction = PublishedAction::new();
static INIT_LOCK: spin::Mutex<Option<Result<(), InitError>>> = spin::Mutex::new(None);
// 0 = unstarted/in progress; 1 = fully installed; 2 = latched failure.
static INIT_STATE: AtomicU8 = AtomicU8::new(0);

/// Signal-safe, nonblocking initialization status. `None` includes installation
/// in progress. Success does not detect a later foreign disposition replacement.
pub fn is_initialized() -> Option<Result<(), InitError>> {
    match INIT_STATE.load(Ordering::Acquire) {
        1 => Some(Ok(())),
        2 => Some(Err(InitError::InstallSignalHandlersFailed)),
        _ => None,
    }
}
/// Install process-wide recovery handlers once, from ordinary thread context.
/// Do not call from a signal handler: initialization may lock and allocate.
///
/// # Safety
/// Coordinate all SIGSEGV/SIGBUS disposition changes with other process users.
/// No foreign owner may change those dispositions during installation or reads.
/// The query/publish/install sequence cannot be atomic with a foreign
/// `sigaction`: an ownership check after it cannot retroactively protect a
/// delivery inside that interval. Coordinate before calling, not just afterward.
/// Failure is latched without retry or rollback; a partially installed handler
/// retains its correct fallback for the process lifetime.
/// Previous callbacks must remain callable for the process lifetime and support
/// signal-context invocation. Do not unload their code. Callers must check the
/// result before reading and must arrange deliverable fault signals on each
/// reading thread. Initialization alone does not prove that thread's mask safe.
///
/// ```compile_fail
/// let _ = pykindasafe::init();
/// ```
pub unsafe fn init() -> Result<(), InitError> {
    let mut g = INIT_LOCK.lock();
    if let Some(prev) = g.clone() {
        return prev;
    }

    let res = init_locked();
    *g = Some(res.clone());
    INIT_STATE.store(if res.is_ok() { 1 } else { 2 }, Ordering::Release);
    res
}

fn init_locked() -> Result<(), InitError> {
    install_handlers_with(
        [
            (libc::SIGSEGV, &FALLBACK_SIGSEGV),
            (libc::SIGBUS, &FALLBACK_SIGBUS),
        ],
        |signal, new, old| unsafe { libc::sigaction(signal, new, old) },
    )
}

fn install_handlers_with(
    slots: [(libc::c_int, &PublishedAction); 2],
    mut sigaction: impl FnMut(libc::c_int, *const libc::sigaction, *mut libc::sigaction) -> libc::c_int,
) -> Result<(), InitError> {
    for (signal, slot) in slots {
        new_signal_handler(signal, crash_handler, slot, &mut sigaction)
            .map_err(|_| InitError::InstallSignalHandlersFailed)?;
    }
    Ok(())
}

/// # Safety
/// `data` must be a valid pointer to `libc::ucontext_t`.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
unsafe extern "C" fn crash_handler(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void,
) {
    unsafe {
        let ctx: *mut libc::ucontext_t = data as *mut libc::ucontext_t;
        let pc = (*ctx).uc_mcontext.gregs[libc::REG_RIP as usize] as usize;
        for x in crate::crash_points().crash_points {
            if x.pc == pc {
                (*ctx).uc_mcontext.gregs[libc::REG_RIP as usize] = (pc + x.skip) as libc::greg_t;
                let reg_idx = match x.signal_reg {
                    crate::Reg::Rax => libc::REG_RAX as usize,
                    crate::Reg::Rdx => libc::REG_RDX as usize,
                };
                (*ctx).uc_mcontext.gregs[reg_idx] = sig as u64 as libc::greg_t;
                return;
            }
        }
        fallback(sig, info, data);
    }
}

/// # Safety
/// `data` must be a valid pointer to `libc::ucontext_t`.
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
unsafe extern "C" fn crash_handler(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void,
) {
    unsafe {
        let ctx: *mut libc::ucontext_t = data as *mut libc::ucontext_t;
        let mctx = (*ctx).uc_mcontext;
        let ss = &mut (*mctx).__ss;
        let pc = ss.__rip as usize;
        for x in crate::crash_points().crash_points {
            if x.pc == pc {
                ss.__rip = (pc + x.skip) as u64;
                match x.signal_reg {
                    crate::Reg::Rax => ss.__rax = sig as u64,
                    crate::Reg::Rdx => ss.__rdx = sig as u64,
                };
                return;
            }
        }
        fallback(sig, info, data);
    }
}

/// # Safety
/// `data` must be a valid pointer to `libc::ucontext_t`.
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
unsafe extern "C" fn crash_handler(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void,
) {
    unsafe {
        let ctx: *mut libc::ucontext_t = data as *mut libc::ucontext_t;
        let pc = (*ctx).uc_mcontext.pc as usize;
        for x in crate::crash_points().crash_points {
            if x.pc == pc {
                (*ctx).uc_mcontext.pc = (pc + x.skip) as u64;
                // libc provides no named constants for aarch64 register indices;
                // mcontext_t.regs is [u64; 31] where index matches register number.
                let reg_idx = match x.signal_reg {
                    crate::Reg::X0 => 0,
                    crate::Reg::X1 => 1,
                };
                (*ctx).uc_mcontext.regs[reg_idx] = sig as u64;
                return;
            }
        }
        fallback(sig, info, data);
    }
}

/// # Safety
/// `data` must be a valid pointer to `libc::ucontext_t`.
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
unsafe extern "C" fn crash_handler(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void,
) {
    unsafe {
        let ctx: *mut libc::ucontext_t = data as *mut libc::ucontext_t;
        let mctx = (*ctx).uc_mcontext;
        let pc = (*mctx).__ss.__pc as usize;
        for x in crate::crash_points().crash_points {
            if x.pc == pc {
                (*mctx).__ss.__pc = (pc + x.skip) as u64;
                // libc provides no named constants for aarch64 register indices;
                // __darwin_arm_thread_state64.__x is [u64; 29] where index matches register number.
                let reg_idx = match x.signal_reg {
                    crate::Reg::X0 => 0,
                    crate::Reg::X1 => 1,
                };
                (*mctx).__ss.__x[reg_idx] = sig as u64;
                return;
            }
        }
        fallback(sig, info, data);
    }
}

fn call_fallback(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void,
    fallback: libc::sigaction,
) {
    if fallback.sa_sigaction == 0 {
        restore_default_ignal_handler(sig);
    } else {
        let handler = unsafe {
            std::mem::transmute::<
                usize,
                extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void),
            >(fallback.sa_sigaction)
        };
        handler(sig, info, data);
    }
}
unsafe fn fallback(sig: libc::c_int, info: *mut libc::siginfo_t, data: *mut libc::c_void) {
    let slot = match sig {
        libc::SIGSEGV => &FALLBACK_SIGSEGV,
        libc::SIGBUS => &FALLBACK_SIGBUS,
        _ => return,
    };
    // A slot is published before its callback can become reachable.
    if let Some(action) = slot.get() {
        call_fallback(sig, info, data, *action);
    }
}

fn new_signal_handler(
    signal: libc::c_int,
    handler: SignalHandler,
    slot: &PublishedAction,
    sigaction: &mut impl FnMut(libc::c_int, *const libc::sigaction, *mut libc::sigaction) -> libc::c_int,
) -> Result<(), ()> {
    let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
    if sigaction(signal, std::ptr::null(), &mut old) != 0 {
        return Err(());
    }
    let mut new = old;
    new.sa_sigaction = handler as usize;
    new.sa_flags |= libc::SA_RESTART | libc::SA_SIGINFO;
    // Publication MUST precede the install syscall. An immediate or concurrent
    // delivery during that syscall can already use the old disposition.
    slot.publish(old);
    if sigaction(signal, &new, std::ptr::null_mut()) != 0 {
        return Err(());
    }
    Ok(())
}

fn restore_default_ignal_handler(sig: libc::c_int) {
    let action: libc::sigaction = unsafe { std::mem::zeroed() };
    unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) };
}

/// Sanity check that kindasafe crash recovery is working.
///
/// Maps a PROT_NONE page, attempts to read it via `crate::u64`,
/// and verifies the read returns an error (SIGSEGV) instead of crashing.
/// Unmaps the page before returning.
///
/// Returns `Ok(())` if the sanity check passes, `Err(SanityCheckFailed)` if
/// the read unexpectedly succeeded (meaning crash recovery is broken).
/// # Safety
/// The installation and delivery obligations of [`crate::u64`] apply. Call only
/// from ordinary thread context: this function maps and unmaps a test page.
pub unsafe fn sanity_check() -> Result<(), InitError> {
    unsafe {
        let page = libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        );
        if page == libc::MAP_FAILED {
            return Err(InitError::SanityCheckFailed);
        }
        let addr = page as u64;
        let result = crate::u64(addr);
        libc::munmap(page, 4096);
        match result {
            Err(_) => Ok(()),
            Ok(_) => Err(InitError::SanityCheckFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_idempotent() {
        assert!(unsafe { init() }.is_ok());
        assert!(unsafe { init() }.is_ok());
        assert!(is_initialized().is_some());
        assert_eq!(is_initialized(), Some(Ok(())));
    }

    #[test]
    fn test_sanity_check() {
        assert!(unsafe { init() }.is_ok());
        assert!(unsafe { sanity_check() }.is_ok());
    }
}

#[cfg(test)]
mod abi_tests {
    #[test]
    fn all_platform_callbacks_use_c_abi() {
        let source = include_str!("recovery.rs");
        // Inspect the inactive targets too; this is not a cross-target build.
        let signature = concat!(r#"unsafe extern "C" fn "#, "crash_handler(");
        assert_eq!(source.matches(signature).count(), 4);
        assert!(!source.contains(concat!("unsafe fn ", "crash_handler(")));
        let _: super::SignalHandler = super::crash_handler;
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    unsafe extern "C" fn previous(_: libc::c_int) {}

    #[test]
    fn snapshots_exist_at_install_entry_and_survive_partial_failure() {
        let previous_address = previous as *const () as usize;
        let segv = PublishedAction::new();
        let bus = PublishedAction::new();
        let mut calls = 0;
        let result = install_handlers_with(
            [(libc::SIGSEGV, &segv), (libc::SIGBUS, &bus)],
            |signal, new, old| {
                calls += 1;
                if new.is_null() {
                    unsafe { (*old).sa_sigaction = previous_address };
                    return 0;
                }
                let slot = if signal == libc::SIGSEGV { &segv } else { &bus };
                // Same production publication boundary, before an installer may
                // expose the handler. Readers on other threads see a complete
                // snapshot without acquiring the initializer's lock.
                std::thread::scope(|scope| {
                    for _ in 0..4 {
                        scope.spawn(|| {
                            assert_eq!(slot.get().unwrap().sa_sigaction, previous_address);
                        });
                    }
                });
                if signal == libc::SIGBUS { -1 } else { 0 }
            },
        );
        assert_eq!(calls, 4);
        assert_eq!(result, Err(InitError::InstallSignalHandlersFailed));
        assert_eq!(segv.get().unwrap().sa_sigaction, previous_address);
        assert_eq!(bus.get().unwrap().sa_sigaction, previous_address);
        // No handler can reference these test-local slots now.
        unsafe {
            drop(Box::from_raw(segv.0.load(Ordering::Relaxed)));
            drop(Box::from_raw(bus.0.load(Ordering::Relaxed)));
        }
    }

    #[test]
    fn failed_query_does_not_publish_or_install() {
        let segv = PublishedAction::new();
        let bus = PublishedAction::new();
        let mut calls = 0;
        assert_eq!(
            install_handlers_with(
                [(libc::SIGSEGV, &segv), (libc::SIGBUS, &bus)],
                |_, new, _| {
                    calls += 1;
                    assert!(new.is_null());
                    -1
                },
            ),
            Err(InitError::InstallSignalHandlersFailed)
        );
        assert_eq!(calls, 1);
        assert!(segv.get().is_none());
        assert!(bus.get().is_none());
    }
}
