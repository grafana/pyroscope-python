use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, Ordering};

#[derive(Debug, PartialEq, Clone)]
pub enum InitError {
    InstallSignalHandlersFailed,
    SanityCheckFailed,
}

/// Why a read must be declined without touching its source address.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RecoveryError {
    /// Includes incomplete or failed installation.
    NotInitialized,
    SignalBlocked {
        signal: libc::c_int,
    },
    /// pthread_sigmask returns an error number directly, not through errno.
    MaskQueryFailed {
        error: libc::c_int,
    },
}

// Access the calling thread's errno directly: no lazy Rust TLS initialization.
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

struct PreserveErrno(libc::c_int);
impl PreserveErrno {
    fn new() -> Self {
        Self(unsafe { *errno_location() })
    }
}
impl Drop for PreserveErrno {
    fn drop(&mut self) {
        unsafe { *errno_location() = self.0 };
    }
}

/// Check recovery availability on the **calling thread**, including a signal
/// handler's currently active mask (not only the interrupted ucontext mask).
/// Uses lock-free status and POSIX async-signal-safe pthread_sigmask/sigismember;
/// it never changes the mask, allocates, waits, or changes errno.
///
/// On failure, a sampler should skip the sample and a worker should decline the
/// read. Do not temporarily unblock fault signals: they may be blocked because
/// a different recovery handler is already active. This result must not be
/// cached across thread changes, mask changes, or signal-handler boundaries.
/// Every public read repeats this check. Success does not establish ownership
/// of foreign memory or detect uncoordinated replacement of our handlers.
pub fn check_recovery() -> Result<(), RecoveryError> {
    check_recovery_with(is_initialized() == Some(Ok(())), |mask| unsafe {
        // With a null set, how is ignored; only query this thread's mask.
        libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), mask)
    })
}

fn check_recovery_with(
    initialized: bool,
    query_mask: impl FnOnce(&mut libc::sigset_t) -> libc::c_int,
) -> Result<(), RecoveryError> {
    let _errno = PreserveErrno::new();
    if !initialized {
        return Err(RecoveryError::NotInitialized);
    }
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    let error = query_mask(&mut mask);
    if error != 0 {
        return Err(RecoveryError::MaskQueryFailed { error });
    }
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        match unsafe { libc::sigismember(&mask, signal) } {
            0 => {}
            1 => return Err(RecoveryError::SignalBlocked { signal }),
            _ => {
                return Err(RecoveryError::MaskQueryFailed {
                    error: unsafe { *errno_location() },
                });
            }
        }
    }
    Ok(())
}

type SignalHandler = unsafe extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void);

// Every cfg-selected platform callback must satisfy the OS C ABI at compile time.
const _: SignalHandler = crash_handler;

// These pointer atomics are lock-free on all supported 64-bit targets. Each
// snapshot is initialized in ordinary thread context and then immutable forever.
// We intentionally retain it even on partial installation failure: a delivery
// may already hold its address. Neither handlers nor initialization reclaim it.
struct PublishedAction(AtomicPtr<libc::sigaction>, AtomicBool);

impl PublishedAction {
    const fn new() -> Self {
        Self(AtomicPtr::new(std::ptr::null_mut()), AtomicBool::new(false))
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
/// Previous ignore/default dispositions and callback signatures are preserved
/// for software-generated deliveries. The kernel applies the previous mask,
/// NODEFER, ONSTACK and RESTART policy. A previous RESETHAND callback is logically
/// consumed by the first fallback, not by a recovered read; later fallbacks take
/// default. OS disposition queries still see our trampoline. Interposition cannot
/// make ignored signals invisible to every interrupted syscall or debugger.
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
/// result before enabling reads. Each read checks the current thread's mask;
/// initialization alone does not prove that thread's mask safe.
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
            if x.pc == pc && is_memory_fault(sig, (*info).si_code) {
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
            if x.pc == pc && is_memory_fault(sig, (*info).si_code) {
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
            if x.pc == pc && is_memory_fault(sig, (*info).si_code) {
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
            if x.pc == pc && is_memory_fault(sig, (*info).si_code) {
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

// Software-generated signals must chain even if they interrupt a read at an
// instruction listed in crash_points. Darwin uses positive SI_USER (0x10001),
// so its memory-fault codes must be selected explicitly.
fn is_memory_fault(sig: libc::c_int, code: libc::c_int) -> bool {
    #[cfg(target_os = "linux")]
    {
        matches!(sig, libc::SIGSEGV | libc::SIGBUS) && code > 0
    }
    #[cfg(target_os = "macos")]
    {
        match sig {
            libc::SIGSEGV => matches!(code, 1 | 2), // SEGV_MAPERR, SEGV_ACCERR
            libc::SIGBUS => matches!(code, 1..=3),  // BUS_ADRALN, BUS_ADRERR, BUS_OBJERR
            _ => false,
        }
    }
}

unsafe fn call_fallback(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    data: *mut libc::c_void,
    action: &libc::sigaction,
    reset: &AtomicBool,
) {
    if action.sa_sigaction == libc::SIG_IGN {
        return;
    }
    if action.sa_sigaction == libc::SIG_DFL
        || (action.sa_flags & libc::SA_RESETHAND != 0 && reset.swap(true, Ordering::AcqRel))
    {
        default_action(sig);
    }
    // The kernel applied the original mask, NODEFER, ONSTACK and RESTART flags
    // when entering our trampoline. RESETHAND is virtualized so recovered reads
    // do not consume the previous callback. Exactly one fallback delivery wins
    // the atomic above; recursive/concurrent/subsequent deliveries take default.
    // A sigaction query sees our trampoline, not this logical one-shot state.
    unsafe {
        if action.sa_flags & libc::SA_SIGINFO != 0 {
            let handler: SignalHandler = std::mem::transmute(action.sa_sigaction);
            handler(sig, info, data);
        } else {
            let handler: unsafe extern "C" fn(libc::c_int) =
                std::mem::transmute(action.sa_sigaction);
            handler(sig);
        }
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
        unsafe { call_fallback(sig, info, data, action, &slot.1) };
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
    // Preserve the application's mask and delivery/restart policy. Never let
    // kernel one-shot handling remove recovery after the first recovered read.
    new.sa_flags = (old.sa_flags & !libc::SA_RESETHAND) | libc::SA_SIGINFO;
    // Publication MUST precede the install syscall. An immediate or concurrent
    // delivery during that syscall can already use the old disposition.
    slot.publish(old);
    if sigaction(signal, &new, std::ptr::null_mut()) != 0 {
        return Err(());
    }
    Ok(())
}

fn default_action(sig: libc::c_int) -> ! {
    // Both signals we handle have fatal defaults. Restore and redeliver the
    // current signal on this thread even for raise()/kill(), which cannot be
    // expected to recur after returning. No lock, allocation or unwinding.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
            libc::_exit(128 + sig);
        }
        if libc::raise(sig) != 0 {
            libc::_exit(128 + sig);
        }
        let mut unblock: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblock);
        libc::sigaddset(&mut unblock, sig);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &unblock, std::ptr::null_mut());
        // Only reachable on an OS failure or an uncoordinated foreign owner.
        libc::_exit(128 + sig);
    }
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
    check_recovery().map_err(|_| InitError::SanityCheckFailed)?;
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
            Err(crate::ReadMemError::Fault { .. }) => Ok(()),
            _ => Err(InitError::SanityCheckFailed),
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

#[cfg(test)]
mod disposition_tests {
    use super::*;

    #[test]
    fn software_deliveries_are_not_memory_faults() {
        #[cfg(target_os = "linux")]
        for code in [libc::SI_USER, libc::SI_TKILL, libc::SI_QUEUE] {
            assert!(!is_memory_fault(libc::SIGBUS, code));
            assert!(!is_memory_fault(libc::SIGSEGV, code));
        }
        #[cfg(target_os = "macos")]
        for code in [0x10001, 0x10002] {
            // Darwin SI_USER and SI_QUEUE
            assert!(!is_memory_fault(libc::SIGBUS, code));
            assert!(!is_memory_fault(libc::SIGSEGV, code));
        }
        assert!(is_memory_fault(libc::SIGBUS, libc::BUS_ADRERR));
        assert!(is_memory_fault(libc::SIGSEGV, 1));
    }
}

#[cfg(test)]
mod mask_query_tests {
    use super::*;

    #[test]
    fn query_failure_is_reported_without_clobbering_errno() {
        unsafe { *errno_location() = libc::EDOM };
        let result = check_recovery_with(true, |_| {
            unsafe { *errno_location() = libc::ERANGE };
            libc::EINVAL
        });
        assert_eq!(
            result,
            Err(RecoveryError::MaskQueryFailed {
                error: libc::EINVAL
            })
        );
        assert_eq!(unsafe { *errno_location() }, libc::EDOM);
    }

    #[test]
    fn incomplete_initialization_declines_without_query() {
        assert_eq!(
            check_recovery_with(false, |_| panic!("must not query")),
            Err(RecoveryError::NotInitialized)
        );
    }
}
