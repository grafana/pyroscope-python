#![no_std]

//! Walks the CPython frame chain of a thread from a signal handler into a caller-provided slice.
//! No allocation, no locks, no recursion.
//!
//! The handler records only code object addresses. Names are resolved later on a normal thread
//! using kindasafe under its recovery and source-borrow preconditions. An address alone does
//! not establish lifetime or object identity; deferred reads can observe reused or changing memory.
//!
//! Initialize kindasafe recovery, invoke [`handler`] from a `SIGPROF` callback, and drain with [`consumer`].
//! Each record contains the interrupted thread's frames, innermost first.
//!
//! Supports CPython 3.10 to 3.14, default (GIL) build, 64-bit targets. The offsets come from the
//! checked-in layouts verified against the target CPython headers by `pyroscope-python-offsets`.
//!
//! Native reads rely on CPython's current-thread ownership of the active frame chain: the
//! interrupted thread is paused, active frames retain their code, and other threads must not
//! mutate that chain. Free-threaded builds and foreign mutation of interpreter internals are
//! unsupported. This is a runtime assumption, not a proof for every CPython interruption point.
//! SDK users must ensure no other code changes SIGSEGV/SIGBUS handlers during
//! initialization or after kindasafe has installed its recovery handlers, including across
//! profiler stops/restarts. Install other handlers before initialization; chained callbacks
//! must remain loaded and support direct chaining. Handler ownership is not checked at runtime:
//! replacing recovery handlers can turn a failed memory read into a process crash.
//! Callers must successfully initialize recovery before unwinding and ensure SIGSEGV/SIGBUS
//! are unblocked on the sampled thread throughout the walk, including when sampling interrupts
//! another signal handler. Initialization and signal masks are not checked while unwinding;
//! violating these preconditions can turn a failed memory read into a process crash.
//! Kindasafe reads use direct memory loads without syscalls on the successful memory-load path.
//! In ASAN builds, a fault recovered during a read is reported to ASAN after recovery.

#[cfg(test)]
extern crate std;

use pyroscope_python_offsets as layout;
use pyroscope_ringbuffer::{Consumer, RingBuffer};

pub use layout::{OB_TYPE, PY_VERSION};

use core::sync::atomic::{AtomicUsize, Ordering};

/// Maximum number of frames visited in one stack, including skipped frames.
pub const MAX_FRAMES: usize = 256;

const EXECUTABLE_MASK: usize = {
    #[cfg(Py_3_14)]
    let tag_bits = layout::EXECUTABLE_TAG_BITS;
    #[cfg(not(Py_3_14))]
    let tag_bits = 0;
    !tag_bits
};

#[cfg(all(Py_3_12, not(Py_3_14)))]
const SKIP_OWNERS: u8 = 1 << layout::FRAME_OWNED_BY_CSTACK;
#[cfg(all(Py_3_14, not(Py_3_15)))]
const SKIP_OWNERS: u8 =
    (1 << layout::FRAME_OWNED_BY_CSTACK) | (1 << layout::FRAME_OWNED_BY_INTERPRETER);
// CPython 3.15 removed C-stack-owned frames. Keep the crate buildable for the
// other profilers; Signal startup still rejects 3.15 before installing a timer.
#[cfg(Py_3_15)]
const SKIP_OWNERS: u8 = 1 << layout::FRAME_OWNED_BY_INTERPRETER;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The calling thread has no Python thread state.
    NoThreadState,
    /// Memory on the chain was unreadable.
    Read,
    /// A Python frame's executable was null or was not a code object.
    InvalidCodeType,
    /// The output slice has no room for the stack.
    Full,
    /// The chain is longer than [`MAX_FRAMES`], or loops.
    TooDeep,
    /// A pointer on the chain overflows the address space.
    Invalid,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Frame {
    /// Address of the `PyCodeObject`.
    pub code: usize,
    /// Signed `f_lasti` code-unit index on 3.10 (cast to `usize`),
    /// `prev_instr` on 3.11 and 3.12, `instr_ptr` from 3.13.
    pub instr: usize,
}

const SLOTS: usize = 1 << 16;

static RECORDS: RingBuffer<Frame, SLOTS> = RingBuffer::new();

/// Reads the records the handler wrote. `None` if another consumer is active.
pub fn consumer() -> Option<Consumer<'static, Frame, SLOTS>> {
    RECORDS.consumer()
}

/// Unwinds the interrupted thread into the ring buffer. Requires successful recovery initialization
/// and both recovery signals to remain unblocked, as documented by [`unwind`].
pub fn handler() {
    // TODO: consider MaybeUninit for frame storage after measuring the initialization cost.
    let mut frames = [Frame::default(); MAX_FRAMES];
    // Unwind errors are counted by the unwinder. Never publish a partial stack.
    let Ok(frames) = unwind(&mut frames) else {
        return;
    };
    let Some(mut producer) = RECORDS.producer() else {
        METRICS.contentions.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if producer.push_slice(frames).is_err() {
        METRICS.buffer_dropped.fetch_add(1, Ordering::Relaxed);
    }
}

fn current_thread_state() -> usize {
    // TODO: PyGILState_GetThisThreadState is not safe on 3.15; find another way to get the tstate.
    // SAFETY: this queries the calling thread's state without requiring the GIL.
    unsafe { pyo3_ffi::PyGILState_GetThisThreadState() as usize }
}

pub struct Metrics {
    contentions: AtomicUsize,
    buffer_dropped: AtomicUsize,
    dropped: AtomicUsize,
    no_thread_state: AtomicUsize,
    invalid_code_type: AtomicUsize,
    read_errors: AtomicUsize,
    /// Stacks dropped because deferred name or line resolution failed.
    pub resolve_error: AtomicUsize,
}

pub static METRICS: Metrics = Metrics {
    contentions: AtomicUsize::new(0),
    buffer_dropped: AtomicUsize::new(0),
    dropped: AtomicUsize::new(0),
    no_thread_state: AtomicUsize::new(0),
    invalid_code_type: AtomicUsize::new(0),
    read_errors: AtomicUsize::new(0),
    resolve_error: AtomicUsize::new(0),
};

/// Recovery initialization must have succeeded and both fault signals must be deliverable on this thread
/// throughout the walk. These caller preconditions are not checked at runtime; a failed
/// memory read can crash the process if they are violated.
///
/// An executable whose type is not `PyCode_Type` fails the entire stack.
/// [`MAX_FRAMES`] bounds the walk, skipped frames included.
/// Writes the calling thread's frames into `frames`, innermost first, and returns that subslice.
/// On error, the caller must discard the partial stack.
/// Unwind errors are counted here; publishing the stack is the caller's responsibility.
pub fn unwind(frames: &mut [Frame]) -> Result<&[Frame], Error> {
    unwind_thread(current_thread_state(), frames)
}

fn unwind_thread(tstate: usize, frames: &mut [Frame]) -> Result<&[Frame], Error> {
    if tstate == 0 {
        METRICS.no_thread_state.fetch_add(1, Ordering::Relaxed);
        return Err(Error::NoThreadState);
    }
    walk(tstate, frames).inspect_err(|error| {
        METRICS.dropped.fetch_add(1, Ordering::Relaxed);
        match error {
            Error::InvalidCodeType => {
                METRICS.invalid_code_type.fetch_add(1, Ordering::Relaxed);
            }
            Error::Read => {
                METRICS.read_errors.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    })
}

fn current_frame(tstate: usize) -> Result<usize, Error> {
    let frame = read_usize(tstate, layout::THREAD_FRAME)?;
    #[cfg(all(Py_3_11, not(Py_3_13)))]
    let frame = read_usize(frame, layout::CFRAME_FRAME)?;
    Ok(frame)
}

fn walk(tstate: usize, frames: &mut [Frame]) -> Result<&[Frame], Error> {
    let code_type = (&raw mut pyo3_ffi::PyCode_Type) as usize;
    let mut frame = current_frame(tstate)?;
    let mut visited = 0;
    let mut pushed = 0;
    while frame != 0 {
        if visited == MAX_FRAMES {
            return Err(Error::TooDeep);
        }
        visited += 1;
        let previous = read_usize(frame, layout::PREVIOUS)?;
        #[cfg(Py_3_12)]
        {
            let owner = read_u8(frame, layout::OWNER)?;
            if SKIP_OWNERS & (1 << owner) != 0 {
                frame = previous;
                continue;
            }
        }
        let executable = read_usize(frame, layout::EXECUTABLE)? & EXECUTABLE_MASK;
        #[cfg(Py_3_11)]
        let instr = read_usize(frame, layout::INSTR)?;
        #[cfg(not(Py_3_11))]
        let instr = i32::from_ne_bytes(read_bytes::<4>(frame, layout::INSTR)?) as isize as usize;

        if executable == 0 || read_usize(executable, OB_TYPE)? != code_type {
            return Err(Error::InvalidCodeType);
        }
        *frames.get_mut(pushed).ok_or(Error::Full)? = Frame {
            code: executable,
            instr,
        };
        pushed += 1;
        frame = previous;
    }
    Ok(&frames[..pushed])
}

#[cfg(Py_3_12)]
fn read_u8(base: usize, offset: usize) -> Result<u8, Error> {
    Ok(read_bytes::<1>(base, offset)?[0])
}

#[cfg(any(Py_3_12, not(Py_3_11)))]
fn read_bytes<const N: usize>(base: usize, offset: usize) -> Result<[u8; N], Error> {
    let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
    addr.checked_add(N).ok_or(Error::Invalid)?;
    let mut out = [0; N];
    // SAFETY: only current-thread frame fields enter here; the runtime ownership assumptions
    // are stated at crate level. Copy exactly the field width, not adjacent padding. Output
    // is separate initialized stack storage, the range is checked, and N is 1 or 4. Recovery
    // initialization, preserved handlers, and unblocked recovery signals are caller
    // preconditions documented at crate level.
    unsafe { kindasafe::slice(&mut out, addr as u64) }.map_err(|_| Error::Read)?;
    Ok(out)
}

fn read_usize(base: usize, offset: usize) -> Result<usize, Error> {
    let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
    addr.checked_add(size_of::<usize>()).ok_or(Error::Invalid)?;
    // SAFETY: these are pointer-sized fields of the interrupted thread's retained frame
    // chain/code objects, under the runtime ownership assumptions documented at crate level.
    // No Rust reference to source memory is created. Recovery initialization, preserved
    // handlers, and unblocked recovery signals are caller preconditions documented at
    // crate level.
    unsafe { kindasafe::u64(addr as u64) }
        .map(|word| word as usize)
        .map_err(|_| Error::Read)
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use std::vec;

    // Serialize tests that inspect the process-wide counters.
    static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn init() {
        // SAFETY: tests preserve kindasafe's handlers and do not install other handlers.
        unsafe { kindasafe_init::init() }.unwrap();
    }

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
        init();
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
        init();
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
        init();
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
}
