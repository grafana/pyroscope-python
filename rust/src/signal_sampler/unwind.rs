//! Walks the CPython frame chain of a thread from a signal handler into a caller-provided slice.
//! No allocation, no locks, no recursion.
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
    /// `prev_instr` on 3.11 and 3.12, `instr_ptr` from 3.13.
    pub instr: usize,
}

fn current_thread_state() -> usize {
    // TODO: PyGILState_GetThisThreadState is not safe on 3.15; find another way to get the tstate.
    // SAFETY: this queries the calling thread's state without requiring the GIL.
    unsafe { pyo3::ffi::PyGILState_GetThisThreadState() as usize }
}

struct Metrics {
    dropped: AtomicUsize,
    no_thread_state: AtomicUsize,
    invalid_code_type: AtomicUsize,
    read_errors: AtomicUsize,
}

static METRICS: Metrics = Metrics {
    dropped: AtomicUsize::new(0),
    no_thread_state: AtomicUsize::new(0),
    invalid_code_type: AtomicUsize::new(0),
    read_errors: AtomicUsize::new(0),
};

/// [`init`] must have succeeded first, or an unreadable address crashes the process.
///
/// An executable whose type is not `PyCode_Type` fails the entire stack.
/// [`MAX_FRAMES`] bounds the walk, skipped frames included.
/// Writes the calling thread's frames into `frames`, innermost first, and returns their count.
/// On error the slice may contain a partial stack, which the caller must discard.
/// Unwind errors are counted here; publishing the stack is the caller's responsibility.
pub fn unwind(frames: &mut [Frame]) -> Result<usize, Error> {
    unwind_thread(current_thread_state(), frames)
}

fn unwind_thread(tstate: usize, frames: &mut [Frame]) -> Result<usize, Error> {
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
    #[cfg(not(Py_3_13))]
    let frame = read_usize(frame, layout::CFRAME_FRAME)?;
    Ok(frame)
}

fn walk(tstate: usize, frames: &mut [Frame]) -> Result<usize, Error> {
    let code_type = (&raw mut pyo3::ffi::PyCode_Type) as usize;
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
        let instr = read_usize(frame, layout::INSTR)?;

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
    Ok(pushed)
}

fn read_u8(base: usize, offset: usize) -> Result<u8, Error> {
    let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
    kindasafe::u64(addr as u64)
        .map(|word| word as u8)
        .map_err(|_| Error::Read)
}

fn read_usize(base: usize, offset: usize) -> Result<usize, Error> {
    let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
    kindasafe::u64(addr as u64)
        .map(|word| word as usize)
        .map_err(|_| Error::Read)
}

#[cfg(all(test, not(miri)))]
mod tests;
