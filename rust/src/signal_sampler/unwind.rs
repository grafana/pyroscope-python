//! Walks the CPython frame chain of a thread from a signal handler and writes it into a
//! [`ringbuffer`](super::ringbuffer) record. No allocation, no locks, no recursion, no buffers.
//!
//! The handler records only code object addresses. Names are resolved later on a normal thread,
//! which must check the code object is still alive, and line numbers from the instruction pointer.
//!
//! [`handler`] has a ready-made `SIGPROF` handler.
//!
//! Supports CPython 3.11 to 3.14, default (GIL) build, 64-bit targets. The offsets come from the
//! checked-in layouts verified against the target CPython headers by `pyroscope-python-offsets`.

pub mod handler;
use pyroscope_python_offsets::unwind as layout;

pub use kindasafe_init::init;
pub use layout::{OB_TYPE, PY_VERSION};

use super::ringbuffer::{Record, RingBuffer};
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
#[cfg(Py_3_14)]
const SKIP_OWNERS: u8 =
    (1 << layout::FRAME_OWNED_BY_CSTACK) | (1 << layout::FRAME_OWNED_BY_INTERPRETER);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The calling thread has no Python thread state.
    NoThreadState,
    /// Another producer holds the ring buffer.
    Contended,
    /// Memory on the chain was unreadable.
    Read,
    /// A Python frame's executable was null or was not a code object.
    InvalidCodeType,
    /// The ring buffer has no room for the stack.
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
    /// `prev_instr` on 3.11 and 3.12, `instr_ptr` from 3.13.
    pub instr: usize,
}

/// `PyGILState_GetThisThreadState`: the thread state of the calling thread, or 0.
pub type ThreadStateGetter = extern "C" fn() -> usize;

pub struct Unwinder {
    code_type: usize,
    thread_state: ThreadStateGetter,
    metrics: Metrics,
}

struct Metrics {
    dropped: AtomicUsize,
    contentions: AtomicUsize,
    no_thread_state: AtomicUsize,
    invalid_code_type: AtomicUsize,
}

impl Unwinder {
    /// [`init`] must have succeeded first, or an unreadable address crashes the process.
    ///
    /// `code_type` is the address of `PyCode_Type`; an executable of another type fails the
    /// entire stack. `thread_state` returns the thread state to walk.
    /// [`MAX_FRAMES`] bounds the walk, skipped frames included.
    pub const fn new(code_type: usize, thread_state: ThreadStateGetter) -> Self {
        Unwinder {
            code_type,
            thread_state,
            metrics: Metrics {
                dropped: AtomicUsize::new(0),
                contentions: AtomicUsize::new(0),
                no_thread_state: AtomicUsize::new(0),
                invalid_code_type: AtomicUsize::new(0),
            },
        }
    }

    /// Commits a record to `records` with a [`Frame`] for each Python frame of the calling
    /// thread, innermost first, and returns the number of frames. On error nothing is committed
    /// and the error is counted.
    pub fn unwind<const N: usize>(&self, records: &RingBuffer<Frame, N>) -> Result<usize, Error> {
        let metrics = &self.metrics;
        let tstate = (self.thread_state)();
        if tstate == 0 {
            metrics.no_thread_state.fetch_add(1, Ordering::Relaxed);
            return Err(Error::NoThreadState);
        }
        let Some(mut producer) = records.producer() else {
            metrics.contentions.fetch_add(1, Ordering::Relaxed);
            return Err(Error::Contended);
        };
        let mut record = producer.record();
        let pushed = self.walk(tstate, &mut record).inspect_err(|error| {
            metrics.dropped.fetch_add(1, Ordering::Relaxed);
            if *error == Error::InvalidCodeType {
                metrics.invalid_code_type.fetch_add(1, Ordering::Relaxed);
            }
        })?;
        if record.commit().is_err() {
            metrics.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(Error::Full);
        }
        Ok(pushed)
    }

    fn current_frame(&self, tstate: usize) -> Result<usize, Error> {
        let frame = self.read_usize(tstate, layout::THREAD_FRAME)?;
        #[cfg(not(Py_3_13))]
        let frame = self.read_usize(frame, layout::CFRAME_FRAME)?;
        Ok(frame)
    }

    fn walk<const N: usize>(
        &self,
        tstate: usize,
        record: &mut Record<'_, '_, Frame, N>,
    ) -> Result<usize, Error> {
        let mut frame = self.current_frame(tstate)?;
        let mut visited = 0;
        let mut pushed = 0;
        while frame != 0 {
            if visited == MAX_FRAMES {
                return Err(Error::TooDeep);
            }
            visited += 1;
            let (previous, python_frame) = self.read_frame(frame)?;
            if let Some(python_frame) = python_frame {
                record.push(python_frame).map_err(|_| Error::Full)?;
                pushed += 1;
            }
            frame = previous;
        }
        Ok(pushed)
    }

    /// Returns the previous frame and, for Python-owned frames, the sampled code and instruction.
    fn read_frame(&self, frame: usize) -> Result<(usize, Option<Frame>), Error> {
        let previous = self.read_usize(frame, layout::PREVIOUS)?;
        #[cfg(Py_3_12)]
        {
            let owner = self.read_u8(frame, layout::OWNER)?;
            if SKIP_OWNERS & (1 << owner) != 0 {
                return Ok((previous, None));
            }
        }
        let executable = self.read_usize(frame, layout::EXECUTABLE)? & EXECUTABLE_MASK;
        let instr = self.read_usize(frame, layout::INSTR)?;

        if executable == 0 || self.read_usize(executable, OB_TYPE)? != self.code_type {
            return Err(Error::InvalidCodeType);
        }
        Ok((
            previous,
            Some(Frame {
                code: executable,
                instr,
            }),
        ))
    }

    fn read_u8(&self, base: usize, offset: usize) -> Result<u8, Error> {
        let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
        kindasafe::u64(addr as u64)
            .map(|word| word as u8)
            .map_err(|_| Error::Read)
    }

    fn read_usize(&self, base: usize, offset: usize) -> Result<usize, Error> {
        let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
        kindasafe::u64(addr as u64)
            .map(|word| word as usize)
            .map_err(|_| Error::Read)
    }
}

#[cfg(all(test, not(miri)))]
mod tests;
