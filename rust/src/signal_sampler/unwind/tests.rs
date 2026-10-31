use super::*;

// Unit tests do not link libpython. Only this symbol's address is used, never its contents.
#[unsafe(export_name = "PyCode_Type")]
static CODE_TYPE: usize = 0;

fn set(memory: &mut [usize], offset: usize, value: usize) {
    assert_eq!(offset % size_of::<usize>(), 0);
    memory[offset / size_of::<usize>()] = value;
}

#[test]
fn invalid_code_and_read_errors_fail_stack_and_count_failures() {
    init().unwrap();
    let code_type = (&raw mut pyo3::ffi::PyCode_Type) as usize;
    let mut code = [0usize; 4];
    set(&mut code, OB_TYPE, code_type);
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

    let unwinder = Unwinder::new();
    let mut output = [Frame::default(); 8];
    for (executable, error) in [
        (wrong_code.as_ptr() as usize, Error::InvalidCodeType),
        (0, Error::InvalidCodeType),
        // Reading ob_type in the unmapped null page fails through kindasafe.
        (8, Error::Read),
    ] {
        set(&mut outer, layout::EXECUTABLE, executable);
        assert_eq!(unwinder.unwind_thread(tstate, &mut output), Err(error));
        // The caller must discard the partial stack on error.
        assert_eq!(output[0].code, code_addr);
    }
    assert_eq!(
        unwinder.metrics.invalid_code_type.load(Ordering::Relaxed),
        2
    );
    assert_eq!(unwinder.metrics.read_errors.load(Ordering::Relaxed), 1);
    assert_eq!(unwinder.metrics.dropped.load(Ordering::Relaxed), 3);

    // The output slice can be reused after a failed walk.
    set(&mut outer, layout::EXECUTABLE, code_addr);
    assert_eq!(unwinder.unwind_thread(tstate, &mut output), Ok(2));
    assert!(output[..2].iter().all(|frame| frame.code == code_addr));
    assert_eq!(
        unwinder.metrics.invalid_code_type.load(Ordering::Relaxed),
        2
    );
    assert_eq!(unwinder.metrics.read_errors.load(Ordering::Relaxed), 1);
    assert_eq!(unwinder.metrics.dropped.load(Ordering::Relaxed), 3);
}

#[test]
fn frame_limit_accepts_256_and_rejects_257() {
    init().unwrap();
    let code_type = (&raw mut pyo3::ffi::PyCode_Type) as usize;
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
    #[cfg(not(Py_3_13))]
    let mut cframe = [0usize; 4];
    #[cfg(not(Py_3_13))]
    let frame = {
        set(&mut cframe, layout::CFRAME_FRAME, frame);
        cframe.as_ptr() as usize
    };
    let mut tstate = [0usize; 16];
    set(&mut tstate, layout::THREAD_FRAME, frame);
    let tstate = tstate.as_ptr() as usize;

    let unwinder = Unwinder::new();
    let mut output = [Frame::default(); 256];
    assert_eq!(unwinder.unwind_thread(tstate, &mut []), Err(Error::Full));
    assert_eq!(
        unwinder.unwind_thread(tstate, &mut output[..255]),
        Err(Error::Full)
    );
    assert_eq!(unwinder.metrics.dropped.load(Ordering::Relaxed), 2);
    assert_eq!(unwinder.unwind_thread(tstate, &mut output), Ok(256));

    set(&mut frames[MAX_FRAMES - 1], layout::PREVIOUS, extra_frame);
    assert_eq!(
        unwinder.unwind_thread(tstate, &mut output),
        Err(Error::TooDeep)
    );
    assert_eq!(unwinder.metrics.dropped.load(Ordering::Relaxed), 3);
}
