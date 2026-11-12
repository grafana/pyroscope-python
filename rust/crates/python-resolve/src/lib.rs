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

use pyroscope_pprof::{StringTable, ffi::FFIFrame};
use pyroscope_python_unwind::{Frame, MAX_FRAMES, METRICS};

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

#[derive(Debug, Clone, Copy)]
pub enum Lines {
    LastInstruction,
    First,
    NoLine,
}

pub trait Types {
    fn code() -> usize;
    fn string() -> usize;
    fn bytes() -> usize;
}

pub struct PYO3Types;

impl Types for PYO3Types {
    fn code() -> usize {
        (&raw mut pyo3_ffi::PyCode_Type) as usize
    }

    fn string() -> usize {
        (&raw mut pyo3_ffi::PyUnicode_Type) as usize
    }

    fn bytes() -> usize {
        (&raw mut pyo3_ffi::PyBytes_Type) as usize
    }
}

pub struct Resolver<T: Types> {
    lines: Lines,
    frames: Vec<FFIFrame>,
    // Allocate bounded decoding storage once, before sampling. Re-reading Python memory
    // on every resolution avoids caching identities of freed or reused objects.
    string_bytes: Vec<u8>,
    text: String,
    _types: PhantomData<T>,
}

impl<T: Types> Resolver<T> {
    pub fn new(lines: Lines) -> Self {
        Self {
            lines,
            frames: Vec::with_capacity(MAX_FRAMES),
            // CPython may store every character as UCS4, even for mostly ASCII text.
            string_bytes: vec![0; 4 * strings::MAX_STRING_BYTES],
            text: String::with_capacity(strings::MAX_STRING_BYTES),
            _types: PhantomData,
        }
    }

    pub fn resolve_stack(
        &mut self,
        frames: &[Frame],
        count: usize,
        strings: &mut StringTable,
    ) -> Result<&[FFIFrame], Error> {
        self.frames.clear();
        let result = (|| {
            if frames.len() > MAX_FRAMES {
                return Err(Error::Length);
            }
            for frame in frames {
                check_type(frame.code, T::code())?;
                strings::read::<T>(
                    word(frame.code, layout::CO_QUALNAME)?,
                    &mut self.string_bytes,
                    &mut self.text,
                )?;
                let name = strings.add(&self.text);
                strings::read::<T>(
                    word(frame.code, layout::CO_FILENAME)?,
                    &mut self.string_bytes,
                    &mut self.text,
                )?;
                let filename = strings.add(&self.text);
                let line = lines::resolve::<T>(frame, self.lines)?;
                self.frames.push(FFIFrame {
                    function_name: (&name).into(),
                    file_name: (&filename).into(),
                    line,
                });
            }
            Ok(())
        })();
        if let Err(error) = result {
            // Discard the entire stack, counting its sampling periods once regardless of depth.
            self.frames.clear();
            METRICS.resolve_error.fetch_add(count, Ordering::Relaxed);
            return Err(error);
        }
        Ok(&self.frames)
    }
}

#[inline(always)]
fn check_type(address: usize, expected: usize) -> Result<(), Error> {
    if address == 0 || word(address, layout::OB_TYPE)? != expected {
        return Err(Error::ObjectType(address));
    }
    Ok(())
}

#[inline(always)]
fn word(address: usize, offset: usize) -> Result<usize, Error> {
    read_u64(address.wrapping_add(offset)).map(|value| value as usize)
}

#[inline(always)]
fn integer(address: usize, offset: usize) -> Result<i32, Error> {
    let addr = address.wrapping_add(offset);
    // SAFETY: recovery and source-borrow assumptions are documented at module level.
    // Read exactly the field width without forming a source reference.
    unsafe { kindasafe::u32(addr as u64) }
        .map(|value| value as i32)
        .map_err(|_| Error::ReadMemory)
}

#[inline(always)]
fn read_u64(addr: usize) -> Result<u64, Error> {
    // SAFETY: recovery and source-borrow assumptions are documented at module level.
    // No Rust reference to the source is created.
    unsafe { kindasafe::u64(addr as u64) }.map_err(|_| Error::ReadMemory)
}

#[inline(always)]
fn copy_bytes(address: usize, len: usize, buffer: &mut [u8]) -> Result<&[u8], Error> {
    let bytes = buffer.get_mut(..len).ok_or(Error::Length)?;
    read_into(address, bytes)?;
    Ok(bytes)
}

#[inline(always)]
fn read_into(addr: usize, out: &mut [u8]) -> Result<(), Error> {
    // SAFETY: recovery and source-borrow assumptions are documented at module level.
    // The destination is initialized, exclusively borrowed storage, separate from the source.
    // Address calculations wrap; kindasafe recovers faults from unreadable source addresses.
    // The copy does not form a source reference.
    unsafe { kindasafe::slice(out, addr as u64) }.map_err(|_| Error::ReadMemory)
}

#[cfg(test)]
static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
