//! Resolves recorded code addresses on a normal thread of the same process. Layouts are
//! checked against CPython headers; names are copied and line tables are read in place.
//! No Python API is called on a sampled address and no reference to Python memory is formed.
//!
//! Session startup initializes recovery. The application must preserve its handlers and keep
//! SIGSEGV/SIGBUS unblocked on the resolver thread. Source ranges must not overlap unrelated
//! live Rust exclusive borrows, including after allocation reuse. These are runtime assumptions,
//! not properties established by a recorded address or by fault recovery. Type and length checks
//! reject many stale reads, but torn reads or address reuse can still misattribute a sample.

use pyroscope_python_offsets as layout;
use std::{marker::PhantomData, sync::atomic::Ordering};

use super::unwind::{Frame, METRICS};
use super::{Lines, ResolvedFrame};

mod lines;
mod strings;
#[cfg(test)]
mod tests;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid instruction address or offset")]
    InstructionOffset,
    #[error("unexpected Python object type at {0:#x}")]
    ObjectType(usize),
    #[error("invalid Python string representation")]
    Unicode,
    #[error("Python object length exceeds the resolver limit")]
    Length,
    #[error("invalid or truncated Python line table")]
    LineTable,
    #[error("failed to read memory")]
    ReadMemory,
}

pub trait Types {
    fn code() -> usize;
    fn string() -> usize;
    fn bytes() -> usize;
}

pub struct PYO3Types;

impl Types for PYO3Types {
    fn code() -> usize {
        (&raw mut pyo3::ffi::PyCode_Type) as usize
    }

    fn string() -> usize {
        (&raw mut pyo3::ffi::PyUnicode_Type) as usize
    }

    fn bytes() -> usize {
        (&raw mut pyo3::ffi::PyBytes_Type) as usize
    }
}

pub struct Resolver<T: Types> {
    lines: Lines,
    _types: PhantomData<T>,
}

impl<T: Types> Resolver<T> {
    pub fn new(lines: Lines) -> Self {
        Self {
            lines,
            _types: PhantomData,
        }
    }

    pub fn resolve_stack(
        &self,
        frames: &[Frame],
        count: usize,
    ) -> Result<Vec<ResolvedFrame>, Error> {
        frames
            .iter()
            .map(|frame| self.resolve_frame(frame))
            .collect::<Result<_, _>>()
            .inspect_err(|_| {
                // Discard the entire stack, counting its sampling periods once regardless of depth.
                METRICS.resolve_error.fetch_add(count, Ordering::Relaxed);
            })
    }

    fn resolve_frame(&self, frame: &Frame) -> Result<ResolvedFrame, Error> {
        check_type(frame.code, T::code())?;
        let name = strings::read::<T>(word(frame.code, layout::CO_QUALNAME)?)?;
        let filename = strings::read::<T>(word(frame.code, layout::CO_FILENAME)?)?;
        let line = lines::resolve::<T>(frame, self.lines)?;
        Ok(ResolvedFrame {
            name,
            filename,
            line,
        })
    }
}

fn check_type(address: usize, expected: usize) -> Result<(), Error> {
    if address == 0 || word(address, layout::OB_TYPE)? != expected {
        return Err(Error::ObjectType(address));
    }
    Ok(())
}

fn word(address: usize, offset: usize) -> Result<usize, Error> {
    read_u64(address.wrapping_add(offset)).map(|value| value as usize)
}

fn integer(address: usize, offset: usize) -> Result<i32, Error> {
    let addr = address.wrapping_add(offset);
    // SAFETY: recovery and source-borrow assumptions are documented at module level.
    // Read exactly the field width without forming a source reference.
    unsafe { kindasafe::u32(addr as u64) }
        .map(|value| value as i32)
        .map_err(|_| Error::ReadMemory)
}

fn read_u64(addr: usize) -> Result<u64, Error> {
    // SAFETY: recovery and source-borrow assumptions are documented at module level.
    // No Rust reference to the source is created.
    unsafe { kindasafe::u64(addr as u64) }.map_err(|_| Error::ReadMemory)
}

fn copy_bytes(address: usize, len: usize, limit: usize) -> Result<Vec<u8>, Error> {
    if len > limit {
        return Err(Error::Length);
    }
    let mut bytes = vec![0; len];
    read_into(address, &mut bytes)?;
    Ok(bytes)
}

fn read_into(addr: usize, out: &mut [u8]) -> Result<(), Error> {
    // SAFETY: recovery and source-borrow assumptions are documented at module level.
    // The destination is initialized, exclusively borrowed storage, separate from the source.
    // Address calculations wrap; kindasafe recovers faults from unreadable source addresses.
    // The copy does not form a source reference.
    unsafe { kindasafe::slice(out, addr as u64) }.map_err(|_| Error::ReadMemory)
}
