//! Walks the CPython frame chain of a thread from a signal handler into a caller-provided slice.
//! No allocation, no locks, no recursion.
//!
//! The handler records only code object addresses. Names are resolved later on a normal thread,
//! which must establish object lifetime before native loads, or use OS-mediated byte copies
//! into owned storage. An address alone does not establish lifetime or object identity.
//!
//! [`handler`] has a ready-made `SIGPROF` handler.
//!
//! Supports CPython 3.10 to 3.15, default (GIL) build, 64-bit targets. The offsets come from the
//! checked-in layouts verified against the target CPython headers by `pyroscope-python-offsets`.
//!
//! Native reads rely on CPython's current-thread ownership of the active frame chain: the
//! interrupted thread is paused, active frames retain their code, and other threads must not
//! mutate that chain. Free-threaded builds and foreign mutation of interpreter internals are
//! unsupported. This is a runtime assumption, not a proof for every CPython interruption point.
//! Applications must coordinate SIGSEGV/SIGBUS disposition changes with initialization/reads;
//! the ownership preflight detects completed takeovers but cannot close a concurrent race.

mod fault_handlers;
pub mod handler;
#[cfg(all(
    any(Py_3_15, all(test, not(miri))),
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod tstate;
use pyroscope_python_offsets as layout;

pub use fault_handlers::{InitError, init, recovery_active};
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
/// 3.15 has no `FRAME_OWNED_BY_CSTACK`.
#[cfg(Py_3_15)]
const SKIP_OWNERS: u8 = 1 << layout::FRAME_OWNED_BY_INTERPRETER;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The calling thread has no Python thread state.
    NoThreadState,
    /// Fault recovery was not initialized or a foreign handler replaced it.
    RecoveryUnavailable,
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

#[cfg(not(Py_3_15))]
fn current_thread_state() -> usize {
    // SAFETY: this queries the calling thread's state without requiring the GIL.
    unsafe { pyo3_ffi::PyGILState_GetThisThreadState() as usize }
}

/// From 3.15 `PyGILState_GetThisThreadState` is not async-signal-safe, see [`tstate`].
#[cfg(all(
    Py_3_15,
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
fn current_thread_state() -> usize {
    // The read goes through kindasafe, so it needs the same guard as `walk`.
    if recovery_active() {
        tstate::gilstate()
    } else {
        0
    }
}

/// The signal sampler refuses to start here.
#[cfg(all(
    Py_3_15,
    not(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
))]
fn current_thread_state() -> usize {
    0
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

/// [`init`] must have succeeded and both fault signals must be deliverable on this thread.
/// Otherwise the sample is declined with [`Error::RecoveryUnavailable`].
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
    if !recovery_active() {
        return Err(Error::RecoveryUnavailable);
    }
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
    if !fault_handlers::signals_unblocked() {
        return Err(Error::Read);
    }
    let mut out = [0; N];
    // SAFETY: only current-thread frame fields enter here; the runtime ownership assumptions
    // are stated at crate level. Copy exactly the field width, not adjacent padding. Output
    // is separate initialized stack storage, the range is checked, and N is 1 or 4. Recovery
    // ownership was checked by walk; the active signal mask was checked above.
    unsafe { kindasafe::slice(&mut out, addr as u64) }.map_err(|_| Error::Read)?;
    Ok(out)
}

fn read_usize(base: usize, offset: usize) -> Result<usize, Error> {
    let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
    addr.checked_add(size_of::<usize>()).ok_or(Error::Invalid)?;
    if !fault_handlers::signals_unblocked() {
        return Err(Error::Read);
    }
    // SAFETY: these are pointer-sized fields of the interrupted thread's retained frame
    // chain/code objects, under the runtime ownership assumptions documented at crate level.
    // No Rust reference to source memory is created. walk checks handler ownership;
    // the check above verifies that this thread can receive both recovery signals.
    unsafe { kindasafe::u64(addr as u64) }
        .map(|word| word as usize)
        .map_err(|_| Error::Read)
}

#[cfg(all(test, not(miri)))]
mod tests;
