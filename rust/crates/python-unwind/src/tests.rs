use super::*;

// Serialize tests that inspect the process-wide counters.
static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Unit tests do not link libpython. Only this symbol's address is used, never its contents.
#[unsafe(export_name = "PyCode_Type")]
static CODE_TYPE: usize = 0;

fn set(memory: &mut [usize], offset: usize, value: usize) {
    assert_eq!(offset % size_of::<usize>(), 0);
    memory[offset / size_of::<usize>()] = value;
}

#[test]
fn invalid_code_and_read_errors_fail_stack_and_count_failures() {
    let _guard = METRICS_LOCK.lock().unwrap();
    let dropped_before = METRICS.dropped.load(Ordering::Relaxed);
    init().unwrap();
    let code_type = (&raw mut pyo3_ffi::PyCode_Type) as usize;
    let mut code = [0usize; 4];
    set(&mut code, OB_TYPE, code_type);
    let invalid_code_type_before = METRICS.invalid_code_type.load(Ordering::Relaxed);
    let read_errors_before = METRICS.read_errors.load(Ordering::Relaxed);
    let wrong_code = [0usize; 4];
    let code_addr = code.as_ptr() as usize;

    let mut outer = [0usize; 16];
    let mut inner = [0usize; 16];
    set(&mut inner, layout::EXECUTABLE, code_addr);
    set(&mut inner, layout::PREVIOUS, outer.as_ptr() as usize);
    let frame = inner.as_ptr() as usize;
    #[cfg(all(Py_3_11, not(Py_3_13)))]
    let mut cframe = [0usize; 4];
    #[cfg(all(Py_3_11, not(Py_3_13)))]
    let frame = {
        set(&mut cframe, layout::CFRAME_FRAME, frame);
        cframe.as_ptr() as usize
    };
    let mut tstate = [0usize; 16];
    set(&mut tstate, layout::THREAD_FRAME, frame);
    let tstate = tstate.as_ptr() as usize;

    let mut output = [Frame::default(); 8];
    for (executable, error) in [
        (wrong_code.as_ptr() as usize, Error::InvalidCodeType),
        (0, Error::InvalidCodeType),
        // Reading ob_type in the unmapped null page fails through kindasafe.
        (8, Error::Read),
    ] {
        set(&mut outer, layout::EXECUTABLE, executable);
        assert_eq!(unwind_thread(tstate, &mut output), Err(error));
    }
    assert_eq!(
        METRICS.invalid_code_type.load(Ordering::Relaxed),
        invalid_code_type_before + 2
    );
    assert_eq!(
        METRICS.read_errors.load(Ordering::Relaxed),
        read_errors_before + 1
    );
    assert_eq!(METRICS.dropped.load(Ordering::Relaxed), dropped_before + 3);

    // The output slice can be reused after a failed walk.
    set(&mut outer, layout::EXECUTABLE, code_addr);
    let stack = unwind_thread(tstate, &mut output).unwrap();
    assert_eq!(stack.len(), 2);
    assert!(stack.iter().all(|frame| frame.code == code_addr));
    assert_eq!(
        METRICS.invalid_code_type.load(Ordering::Relaxed),
        invalid_code_type_before + 2
    );
    assert_eq!(
        METRICS.read_errors.load(Ordering::Relaxed),
        read_errors_before + 1
    );
    assert_eq!(METRICS.dropped.load(Ordering::Relaxed), dropped_before + 3);
}

#[test]
fn frame_limit_accepts_256_and_rejects_257() {
    let _guard = METRICS_LOCK.lock().unwrap();
    let dropped_before = METRICS.dropped.load(Ordering::Relaxed);
    init().unwrap();
    let code_type = (&raw mut pyo3_ffi::PyCode_Type) as usize;
    let mut code = [0usize; 4];
    set(&mut code, OB_TYPE, code_type);
    let mut frames = vec![[0usize; 16]; MAX_FRAMES + 1];
    for index in 0..frames.len() {
        let previous = frames
            .get(index + 1)
            .map_or(0, |frame| frame.as_ptr() as usize);
        set(&mut frames[index], layout::PREVIOUS, previous);
        set(
            &mut frames[index],
            layout::EXECUTABLE,
            code.as_ptr() as usize,
        );
    }
    let extra_frame = frames[MAX_FRAMES].as_ptr() as usize;
    set(&mut frames[MAX_FRAMES - 1], layout::PREVIOUS, 0);
    let frame = frames[0].as_ptr() as usize;
    #[cfg(all(Py_3_11, not(Py_3_13)))]
    let mut cframe = [0usize; 4];
    #[cfg(all(Py_3_11, not(Py_3_13)))]
    let frame = {
        set(&mut cframe, layout::CFRAME_FRAME, frame);
        cframe.as_ptr() as usize
    };
    let mut tstate = [0usize; 16];
    set(&mut tstate, layout::THREAD_FRAME, frame);
    let tstate = tstate.as_ptr() as usize;

    let mut output = [Frame::default(); 256];
    assert_eq!(unwind_thread(tstate, &mut []), Err(Error::Full));
    assert_eq!(unwind_thread(tstate, &mut output[..255]), Err(Error::Full));
    assert_eq!(METRICS.dropped.load(Ordering::Relaxed), dropped_before + 2);
    let stack = unwind_thread(tstate, &mut output).unwrap();
    assert_eq!(stack.len(), MAX_FRAMES);
    assert!(stack.iter().all(|frame| {
        *frame
            == Frame {
                code: code.as_ptr() as usize,
                instr: 0,
            }
    }));

    set(&mut frames[MAX_FRAMES - 1], layout::PREVIOUS, extra_frame);
    assert_eq!(unwind_thread(tstate, &mut output), Err(Error::TooDeep));
    assert_eq!(METRICS.dropped.load(Ordering::Relaxed), dropped_before + 3);
}

#[cfg(not(Py_3_11))]
#[test]
fn python_310_reads_signed_lasti_and_returns_the_output_subslice() {
    let _guard = METRICS_LOCK.lock().unwrap();
    init().unwrap();
    let mut code = [0usize; 4];
    set(
        &mut code,
        OB_TYPE,
        (&raw mut pyo3_ffi::PyCode_Type) as usize,
    );
    let mut frame = [0usize; 16];
    set(&mut frame, layout::EXECUTABLE, code.as_ptr() as usize);
    let mut tstate = [0usize; 16];
    set(&mut tstate, layout::THREAD_FRAME, frame.as_ptr() as usize);
    let mut output = [Frame {
        code: 123,
        instr: 456,
    }; 4];
    let output_ptr = output.as_ptr();
    for lasti in [-1i32, 0, 17] {
        // f_lasti is a 32-bit signed field; the adjacent field must not become part of it.
        set(
            &mut frame,
            layout::INSTR,
            (0x12345678usize << 32) | lasti as u32 as usize,
        );
        let stack = unwind_thread(tstate.as_ptr() as usize, &mut output).unwrap();
        assert_eq!(stack.as_ptr(), output_ptr);
        assert_eq!(
            stack,
            &[Frame {
                code: code.as_ptr() as usize,
                instr: lasti as isize as usize
            }]
        );
        assert_eq!(
            output[1..],
            [Frame {
                code: 123,
                instr: 456
            }; 3]
        );
    }
}

#[test]
fn fault_handler_takeover_disables_reads_and_restart() {
    const CHILD_SIGNAL: &str = "PYROSCOPE_TEST_REPLACED_SIGNAL";
    if let Ok(signal) = std::env::var(CHILD_SIGNAL) {
        init().unwrap();
        assert!(recovery_active());
        let signal = signal.parse::<libc::c_int>().unwrap();
        unsafe { libc::signal(signal, libc::SIG_DFL) };
        // No read of this invalid thread state may occur after either handler is replaced.
        assert_eq!(
            unwind_thread(1, &mut [Frame::default()]),
            Err(Error::RecoveryUnavailable)
        );
        assert!(!recovery_active());
        assert!(init().is_err());
        let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::sigaction(signal, std::ptr::null(), &mut current) },
            0
        );
        assert_eq!(current.sa_sigaction, libc::SIG_DFL);
        return;
    }
    // Isolate changes to process-wide dispositions from the other tests and each other.
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::fault_handler_takeover_disables_reads_and_restart",
            ])
            .env(CHILD_SIGNAL, signal.to_string())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "child failed after takeover of signal {signal}: {status}"
        );
    }
}

#[test]
fn blocked_fault_signal_skips_sample_without_latching_takeover() {
    let _guard = METRICS_LOCK.lock().unwrap();
    init().unwrap();
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, signal);
            assert_eq!(
                libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut previous),
                0
            );
        }
        // A known invalid source must never be touched while recovery is blocked.
        let mut frames = [Frame::default()];
        let result = unwind_thread(1, &mut frames);
        let word_result = read_usize(1, 0);
        #[cfg(any(Py_3_12, not(Py_3_11)))]
        let byte_result = read_bytes::<1>(1, 0);
        let active = recovery_active();
        let restart = init();
        assert_eq!(
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) },
            0
        );
        assert_eq!(result, Err(Error::RecoveryUnavailable));
        assert_eq!(word_result, Err(Error::Read));
        #[cfg(any(Py_3_12, not(Py_3_11)))]
        assert_eq!(byte_result, Err(Error::Read));
        assert!(!active);
        assert!(restart.is_ok());
        assert!(
            recovery_active(),
            "temporary mask must not disable later samples"
        );
    }
}
