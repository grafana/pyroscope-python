use super::*;
use crate::signal_sampler::ringbuffer::PopError;
use std::cell::Cell;

thread_local! {
    static TSTATE: Cell<usize> = const { Cell::new(0) };
}

extern "C" fn thread_state() -> usize {
    TSTATE.get()
}

fn set(memory: &mut [usize], offset: usize, value: usize) {
    assert_eq!(offset % size_of::<usize>(), 0);
    memory[offset / size_of::<usize>()] = value;
}

#[test]
fn invalid_code_type_discards_stack_and_counts_failure() {
    init().unwrap();
    const CODE_TYPE: usize = 0x1000;
    let mut code = [0usize; 4];
    set(&mut code, OB_TYPE, CODE_TYPE);
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
    TSTATE.set(tstate.as_ptr() as usize);

    let unwinder = Unwinder::new(CODE_TYPE, thread_state, 8);
    let records = RingBuffer::<Frame, 8>::new();
    let mut output = [Frame::default(); 8];
    for executable in [wrong_code.as_ptr() as usize, 0] {
        set(&mut outer, layout::EXECUTABLE, executable);
        assert_eq!(unwinder.unwind(&records), Err(Error::InvalidCodeType));
        // The valid inner frame must not be published as a truncated stack.
        assert_eq!(
            records.consumer().unwrap().pop_slice(&mut output),
            Err(PopError::Empty)
        );
    }
    assert_eq!(
        unwinder.metrics.invalid_code_type.load(Ordering::Relaxed),
        2
    );
    assert_eq!(unwinder.metrics.dropped.load(Ordering::Relaxed), 2);

    // A failed walk releases the producer and leaves the buffer usable.
    set(&mut outer, layout::EXECUTABLE, code_addr);
    assert_eq!(unwinder.unwind(&records), Ok(2));
    assert_eq!(records.consumer().unwrap().pop_slice(&mut output), Ok(2));
    assert!(output[..2].iter().all(|frame| frame.code == code_addr));
    assert_eq!(
        unwinder.metrics.invalid_code_type.load(Ordering::Relaxed),
        2
    );
    assert_eq!(unwinder.metrics.dropped.load(Ordering::Relaxed), 2);
    TSTATE.set(0);
}
