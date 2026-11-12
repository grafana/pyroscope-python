#![no_std]

//! Walks the CPython frame chain of a thread from a signal handler into a caller-provided slice.
//! No allocation, no locks, no recursion.
//!
//! The handler records only code object addresses. Names are resolved later on a normal thread
//! using kindasafe under its recovery and source-borrow preconditions. An address alone does
//! not establish lifetime or object identity; deferred reads can observe reused or changing memory.
//!
//! Initialize kindasafe recovery, invoke [`handler`] from a `SIGPROF` callback, and drain with [`consumer`].
//! Each record contains the interrupted thread's frames, innermost first, and its sampling-period count.
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
    /// The output slice has no room, or the chain exceeds [`MAX_FRAMES`] (including loops).
    Truncated,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Frame {
    /// Address of the `PyCodeObject`.
    pub code: usize,
    /// Signed `f_lasti` code-unit index on 3.10 (cast to `usize`),
    /// `prev_instr` on 3.11 and 3.12, `instr_ptr` from 3.13.
    pub instr: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleMetadata {
    /// Number of sampling periods represented by this delivery, including overruns.
    pub count: usize,
}

const SLOTS: usize = 1 << 16;

static RECORDS: RingBuffer<Frame, SLOTS, SampleMetadata> = RingBuffer::new();

/// Reads the records the handler wrote. `None` if another consumer is active.
pub fn consumer() -> Option<Consumer<'static, Frame, SLOTS, SampleMetadata>> {
    RECORDS.consumer()
}

/// Unwinds the interrupted thread into the ring buffer. Requires successful recovery initialization
/// and both recovery signals to remain unblocked, as documented by [`unwind`].
/// `metadata.count` is the number of sampling periods represented by this delivery (including overruns).
/// Errors and buffer drops retain the same count as a successfully published stack.
pub fn handler(metadata: SampleMetadata) {
    // TODO: consider MaybeUninit for frame storage after measuring the initialization cost.
    let mut frames = [Frame::default(); MAX_FRAMES];
    // Unwind errors are counted by the unwinder. Never publish a partial stack.
    let Ok(frames) = unwind_thread(current_thread_state(), &mut frames, metadata.count) else {
        return;
    };
    record_stack(frames, metadata);
}

fn record_stack(frames: &[Frame], metadata: SampleMetadata) {
    let Some(mut producer) = RECORDS.producer() else {
        METRICS
            .contentions
            .fetch_add(metadata.count, Ordering::Relaxed);
        return;
    };
    if producer.push_slice(frames, metadata).is_err() {
        METRICS
            .buffer_dropped
            .fetch_add(metadata.count, Ordering::Relaxed);
    }
}

#[inline(always)]
fn current_thread_state() -> usize {
    // TODO: PyGILState_GetThisThreadState is not safe on 3.15; find another way to get the tstate.
    // SAFETY: this queries the calling thread's state without requiring the GIL.
    unsafe { pyo3_ffi::PyGILState_GetThisThreadState() as usize }
}

#[derive(Default)]
pub struct Metrics {
    contentions: AtomicUsize,
    buffer_dropped: AtomicUsize,
    truncated: AtomicUsize,
    no_thread_state: AtomicUsize,
    invalid_code_type: AtomicUsize,
    read_errors: AtomicUsize,
    /// Stacks dropped because deferred name or line resolution failed.
    pub resolve_error: AtomicUsize,
}

pub static METRICS: Metrics = Metrics {
    contentions: AtomicUsize::new(0),
    buffer_dropped: AtomicUsize::new(0),
    truncated: AtomicUsize::new(0),
    no_thread_state: AtomicUsize::new(0),
    invalid_code_type: AtomicUsize::new(0),
    read_errors: AtomicUsize::new(0),
    resolve_error: AtomicUsize::new(0),
};

impl Metrics {
    /// Takes lost sampling-period counts since the previous drain, one reason per lost stack.
    /// Concurrent increments remain in this batch or the next one.
    pub fn take_drop_counts(&self) -> [(&'static str, usize); 7] {
        [
            ("contentions", &self.contentions),
            ("buffer_dropped", &self.buffer_dropped),
            ("no_thread_state", &self.no_thread_state),
            ("invalid_code_type", &self.invalid_code_type),
            ("read_errors", &self.read_errors),
            ("truncated", &self.truncated),
            ("resolve_error", &self.resolve_error),
        ]
        .map(|(reason, count)| (reason, count.swap(0, Ordering::Relaxed)))
    }
}

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
    unwind_thread(current_thread_state(), frames, 1)
}

fn unwind_thread(tstate: usize, frames: &mut [Frame], count: usize) -> Result<&[Frame], Error> {
    let result = if tstate == 0 {
        Err(Error::NoThreadState)
    } else {
        walk(tstate, frames)
    };
    result.inspect_err(|error| {
        let counter = match error {
            Error::NoThreadState => &METRICS.no_thread_state,
            Error::InvalidCodeType => &METRICS.invalid_code_type,
            Error::Read => &METRICS.read_errors,
            Error::Truncated => &METRICS.truncated,
        };
        counter.fetch_add(count, Ordering::Relaxed);
    })
}

#[inline(always)]
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
            return Err(Error::Truncated);
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
        let instr = read_u32(frame, layout::INSTR)? as i32 as isize as usize;

        if executable == 0 || read_usize(executable, OB_TYPE)? != code_type {
            return Err(Error::InvalidCodeType);
        }
        *frames.get_mut(pushed).ok_or(Error::Truncated)? = Frame {
            code: executable,
            instr,
        };
        pushed += 1;
        frame = previous;
    }
    Ok(&frames[..pushed])
}

#[cfg(Py_3_12)]
#[inline(always)]
fn read_u8(base: usize, offset: usize) -> Result<u8, Error> {
    let addr = base.wrapping_add(offset);
    // SAFETY: current-thread frame fields are read under the ownership and recovery
    // preconditions documented at crate level, without forming a source reference.
    unsafe { kindasafe::u8(addr as u64) }.map_err(|_| Error::Read)
}

#[cfg(not(Py_3_11))]
#[inline(always)]
fn read_u32(base: usize, offset: usize) -> Result<u32, Error> {
    let addr = base.wrapping_add(offset);
    // SAFETY: current-thread frame fields are read under the ownership and recovery
    // preconditions documented at crate level, without forming a source reference.
    unsafe { kindasafe::u32(addr as u64) }.map_err(|_| Error::Read)
}

#[inline(always)]
fn read_usize(base: usize, offset: usize) -> Result<usize, Error> {
    let addr = base.wrapping_add(offset);
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
    fn drop_counts_are_consumed_once_per_profile() {
        let metrics = Metrics::default();
        for (counter, count) in [
            (&metrics.contentions, 1),
            (&metrics.buffer_dropped, 2),
            (&metrics.no_thread_state, 3),
            (&metrics.invalid_code_type, 4),
            (&metrics.read_errors, 5),
            (&metrics.truncated, 13),
            (&metrics.resolve_error, 9),
        ] {
            counter.fetch_add(count, Ordering::Relaxed);
        }
        let expected = [
            ("contentions", 1),
            ("buffer_dropped", 2),
            ("no_thread_state", 3),
            ("invalid_code_type", 4),
            ("read_errors", 5),
            ("truncated", 13),
            ("resolve_error", 9),
        ];
        assert_eq!(metrics.take_drop_counts(), expected);
        assert_eq!(
            metrics.take_drop_counts(),
            expected.map(|(name, _)| (name, 0))
        );
        metrics.resolve_error.fetch_add(2, Ordering::Relaxed);
        assert_eq!(
            metrics.take_drop_counts(),
            expected.map(|(name, _)| { (name, if name == "resolve_error" { 2 } else { 0 }) })
        );
    }

    #[test]
    fn missing_thread_and_invalid_address_have_separate_counts() {
        init();
        let _guard = METRICS_LOCK.lock().unwrap();
        let no_thread_before = METRICS.no_thread_state.load(Ordering::Relaxed);
        let read_before = METRICS.read_errors.load(Ordering::Relaxed);
        let mut output = [Frame::default(); 1];
        assert_eq!(unwind_thread(0, &mut output, 7), Err(Error::NoThreadState));
        assert_eq!(unwind_thread(usize::MAX, &mut output, 11), Err(Error::Read));
        assert_eq!(
            METRICS.no_thread_state.load(Ordering::Relaxed),
            no_thread_before + 7
        );
        assert_eq!(
            METRICS.read_errors.load(Ordering::Relaxed),
            read_before + 11
        );
    }

    #[test]
    fn address_overflows_are_recovered_as_read_errors() {
        init();
        for offset in [0, 1] {
            assert_eq!(read_usize(usize::MAX, offset), Err(Error::Read));
            #[cfg(Py_3_12)]
            assert_eq!(read_u8(usize::MAX, offset), Err(Error::Read));
            #[cfg(not(Py_3_11))]
            assert_eq!(read_u32(usize::MAX, offset), Err(Error::Read));
        }
        let value = 0x8123_4567_89ab_cdefusize;
        assert_eq!(read_usize(&raw const value as usize, 0), Ok(value));
        #[cfg(Py_3_12)]
        {
            let value = 0xffu8;
            assert_eq!(read_u8(&raw const value as usize, 0), Ok(value));
        }
        #[cfg(not(Py_3_11))]
        {
            let value = 0x8000_0000u32;
            assert_eq!(read_u32(&raw const value as usize, 0), Ok(value));
        }
    }

    #[test]
    fn recorded_stacks_and_buffer_drops_preserve_period_counts() {
        let _guard = METRICS_LOCK.lock().unwrap();
        let frames = [Frame {
            code: 123,
            instr: 456,
        }; MAX_FRAMES];
        let mut output = [Frame::default(); MAX_FRAMES];
        let mut consumer = consumer().unwrap();

        record_stack(&frames, SampleMetadata { count: 17 });
        assert_eq!(
            consumer.pop_slice(&mut output),
            Ok((MAX_FRAMES, SampleMetadata { count: 17 }))
        );
        assert_eq!(output, frames);

        let before = METRICS.contentions.load(Ordering::Relaxed);
        let mut producer = RECORDS.producer().unwrap();
        record_stack(&frames, SampleMetadata { count: 19 });
        assert_eq!(METRICS.contentions.load(Ordering::Relaxed), before + 19);

        while producer
            .push_slice(&frames, SampleMetadata { count: 1 })
            .is_ok()
        {}
        drop(producer);
        let before = METRICS.buffer_dropped.load(Ordering::Relaxed);
        record_stack(&frames, SampleMetadata { count: 23 });
        assert_eq!(METRICS.buffer_dropped.load(Ordering::Relaxed), before + 23);
        while consumer.pop_slice(&mut output).is_ok() {}
    }

    #[test]
    fn invalid_code_and_read_errors_fail_stack_and_count_failures() {
        let _guard = METRICS_LOCK.lock().unwrap();
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
            assert_eq!(unwind_thread(tstate, &mut output, 1), Err(error));
        }
        assert_eq!(
            METRICS.invalid_code_type.load(Ordering::Relaxed),
            invalid_code_type_before + 2
        );
        assert_eq!(
            METRICS.read_errors.load(Ordering::Relaxed),
            read_errors_before + 1
        );

        // The output slice can be reused after a failed walk.
        set(&mut outer, layout::EXECUTABLE, code_addr);
        let stack = unwind_thread(tstate, &mut output, 1).unwrap();
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
    }

    #[test]
    fn frame_limit_accepts_256_and_rejects_257() {
        let _guard = METRICS_LOCK.lock().unwrap();
        let truncated_before = METRICS.truncated.load(Ordering::Relaxed);
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
        assert_eq!(unwind_thread(tstate, &mut [], 1), Err(Error::Truncated));
        assert_eq!(
            unwind_thread(tstate, &mut output[..255], 1),
            Err(Error::Truncated)
        );
        assert_eq!(
            METRICS.truncated.load(Ordering::Relaxed),
            truncated_before + 2
        );
        let stack = unwind_thread(tstate, &mut output, 1).unwrap();
        assert_eq!(stack.len(), MAX_FRAMES);
        assert!(stack.iter().all(|frame| {
            *frame
                == Frame {
                    code: code.as_ptr() as usize,
                    instr: 0,
                }
        }));

        set(&mut frames[MAX_FRAMES - 1], layout::PREVIOUS, extra_frame);
        assert_eq!(unwind_thread(tstate, &mut output, 1), Err(Error::Truncated));
        assert_eq!(
            METRICS.truncated.load(Ordering::Relaxed),
            truncated_before + 3
        );
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
            let stack = unwind_thread(tstate.as_ptr() as usize, &mut output, 1).unwrap();
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
