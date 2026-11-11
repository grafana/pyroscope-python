// Frame resolution adapted from py-spy (MIT); see LICENSE.py-spy.
//! Resolves the code objects a signal handler recorded into names, from a normal thread of the
//! same process. Kindasafe reads potentially stale addresses into untrusted bytes. The resolver
//! validates code and string types and bounds copied lengths, then decodes names and lines.
//! Torn reads or a reused address can still produce a misattributed sample.
//!
//! Session startup initializes recovery. The application must preserve its handlers and keep
//! SIGSEGV/SIGBUS unblocked on the resolver thread. Source ranges must not overlap unrelated
//! live Rust exclusive borrows, including after allocation reuse. These are runtime assumptions,
//! not properties established by a recorded address or by fault recovery.

use anyhow::Context;
use py_spy::{
    python_bindings as bindings,
    python_interpreters::{BytesObject, CodeObject, FrameObject, StringObject},
    python_process_info::{TypeChecks, Types},
};
use remoteprocess::ProcessMemory;
use std::sync::atomic::Ordering;

use super::unwind::{Frame, METRICS};
use super::{Lines, ResolvedFrame};

#[cfg(not(Py_3_11))]
use bindings::v3_10_0::_frame as PythonFrame;
#[cfg(all(Py_3_11, not(Py_3_12)))]
use bindings::v3_11_0::_PyInterpreterFrame as PythonFrame;
#[cfg(all(Py_3_12, not(Py_3_13)))]
use bindings::v3_12_0::_PyInterpreterFrame as PythonFrame;
#[cfg(all(Py_3_13, not(Py_3_14)))]
use bindings::v3_13_0::_PyInterpreterFrame as PythonFrame;
#[cfg(all(Py_3_14, not(Py_3_15)))]
use bindings::v3_14_0::_PyInterpreterFrame as PythonFrame;
#[cfg(Py_3_15)]
use bindings::v3_15_0::_PyInterpreterFrame as PythonFrame;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[cfg(Py_3_11)]
    #[error("instruction {instr:#x} precedes code object {code:#x}")]
    InstructionBeforeCode { instr: usize, code: usize },
    #[error("instruction offset is out of range")]
    InstructionOffset(#[from] std::num::TryFromIntError),
    #[error("{0:#}")]
    PySpy(#[from] anyhow::Error),
    #[error("memory address range overflows: {len} bytes at {addr:#x}")]
    AddressRangeOverflow { addr: usize, len: usize },
    #[error("failed to read {len} bytes at {addr:#x}: {fault:?}")]
    ReadMemory {
        addr: usize,
        len: usize,
        fault: kindasafe::ReadMemError,
    },
}

pub struct Resolver {
    types: TypeChecks,
    lines: Lines,
}

impl Resolver {
    pub fn new(types: Types, lines: Lines) -> Self {
        Self {
            types: TypeChecks::Strict(types),
            lines,
        }
    }

    pub fn resolve_stack(&mut self, frames: &[Frame]) -> Result<Vec<ResolvedFrame>, Error> {
        self.resolve_stack_inner(frames).inspect_err(|_| {
            METRICS.resolve_error.fetch_add(1, Ordering::Relaxed);
        })
    }

    fn resolve_stack_inner(&mut self, frames: &[Frame]) -> Result<Vec<ResolvedFrame>, Error> {
        let mut resolved: Vec<ResolvedFrame> = Vec::with_capacity(frames.len());
        for frame in frames {
            let lasti = match self.lines {
                Lines::LastInstruction => instruction_offset(frame)?,
                Lines::First | Lines::NoLine => 0,
            };
            let code = Memory
                .copy_pointer(frame.code as *const <PythonFrame as FrameObject>::CodeObject)
                .context("Failed to copy PyCodeObject")?;
            self.types.check_code(&code)?;
            let filename = copy_string(code.filename(), &Memory, &self.types);
            let name = match code.qualname() {
                Some(ptr) => copy_string(ptr, &Memory, &self.types)
                    .or_else(|_| copy_string(code.name(), &Memory, &self.types)),
                None => copy_string(code.name(), &Memory, &self.types),
            };
            // Match py-spy's handling of torn reads and interpreter shim frames.
            let (Ok(filename), Ok(name)) = (filename, name) else {
                if let Some(previous) = resolved.last_mut() {
                    previous.is_shim_entry = true;
                }
                continue;
            };
            if filename.is_empty() || filename == "<shim>" {
                if let Some(previous) = resolved.last_mut() {
                    previous.is_shim_entry = true;
                }
                continue;
            }
            let line = match self.lines {
                Lines::NoLine => 0,
                Lines::First => code.first_lineno(),
                Lines::LastInstruction => match copy_bytes(code.line_table(), &Memory) {
                    Ok(table) => code.get_line_number(lasti, &table),
                    Err(error) => {
                        log::warn!(
                            "Failed to get line number from {}.{}: {}",
                            filename,
                            name,
                            error
                        );
                        0
                    }
                },
            };
            resolved.push(ResolvedFrame {
                name,
                filename,
                line,
                short_filename: None,
                module: None,
                locals: None,
                is_entry: false,
                is_shim_entry: false,
            });
        }
        if let Some(last) = resolved.last_mut() {
            last.is_shim_entry = true;
        }
        Ok(resolved)
    }
}

fn instruction_offset(frame: &Frame) -> Result<i32, Error> {
    // py-spy expects f_lasti code units on 3.10 and a byte offset from the
    // beginning of the code object on 3.11+, including its header.
    #[cfg(not(Py_3_11))]
    let offset = i32::try_from(frame.instr as isize);
    #[cfg(Py_3_11)]
    let offset = i32::try_from(frame.instr.checked_sub(frame.code).ok_or(
        Error::InstructionBeforeCode {
            instr: frame.instr,
            code: frame.code,
        },
    )?);
    offset.map_err(Error::InstructionOffset)
}

/// Supplies py-spy's decoder with fault-recovering reads of this process.
struct Memory;

impl ProcessMemory for Memory {
    fn read(&self, addr: usize, out: &mut [u8]) -> Result<(), remoteprocess::Error> {
        addr.checked_add(out.len()).ok_or_else(|| {
            std::io::Error::other(Error::AddressRangeOverflow {
                addr,
                len: out.len(),
            })
        })?;
        if out.is_empty() {
            return Ok(());
        }
        // SAFETY: recovery and source-borrow assumptions are documented at module level.
        // The destination is separate, initialized, exclusively borrowed storage. The source
        // range is checked for overflow and copied without constructing a source reference.
        unsafe { kindasafe::slice(out, addr as u64) }.map_err(|fault| {
            std::io::Error::other(Error::ReadMemory {
                addr,
                len: out.len(),
                fault,
            })
            .into()
        })
    }
}

pub fn copy_string<T: StringObject, P: ProcessMemory>(
    ptr: *const T,
    process: &P,
    type_checks: &TypeChecks,
) -> Result<String, anyhow::Error> {
    let obj = T::copy_header(ptr, process)?;
    type_checks.check_string(process, &obj)?;
    let length = obj.size()?;
    if length >= 4096 {
        return Err(anyhow::format_err!(
            "Refusing to copy {} chars of a string",
            length
        ));
    }

    let kind = obj.kind();
    if !matches!(kind, 1 | 2 | 4) {
        return Err(anyhow::format_err!("Unknown string kind {}", kind));
    }
    let ascii = obj.ascii();
    if ascii && kind != 1 {
        return Err(anyhow::format_err!(
            "ASCII string has non-one-byte kind {}",
            kind
        ));
    }
    if length == 0 {
        return Ok(String::new());
    }

    // Length is below 4096 and kind is at most 4, so this cannot overflow.
    let byte_length = length * kind as usize;
    let address = obj.address(ptr as usize)?;
    address
        .checked_add(byte_length)
        .ok_or_else(|| anyhow::format_err!("String range overflow"))?;
    let bytes = process.copy(address, byte_length)?;

    match (kind, ascii) {
        (4, _) => {
            // Decode UCS-4 via u32 + char::from_u32. Casting process memory to
            // &[char] is UB: char requires a Unicode scalar value, and the
            // buffer is only 1-byte aligned. See issue #870.
            let mut out = String::with_capacity(bytes.len() / 4);
            for chunk in bytes.chunks_exact(4) {
                let code = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                match char::from_u32(code) {
                    Some(c) => out.push(c),
                    None => {
                        return Err(anyhow::format_err!(
                            "Invalid Unicode code point in UCS-4 string: {:#x}",
                            code
                        ));
                    }
                }
            }
            Ok(out)
        }
        (2, _) => {
            // Avoid casting an unaligned Vec<u8> buffer to &[u16].
            let mut units = Vec::with_capacity(bytes.len() / 2);
            for chunk in bytes.chunks_exact(2) {
                units.push(u16::from_ne_bytes([chunk[0], chunk[1]]));
            }
            Ok(String::from_utf16(&units)?)
        }
        (1, true) => Ok(String::from_utf8(bytes)?),
        (1, false) => Ok(bytes.iter().map(|&b| b as char).collect()),
        _ => Err(anyhow::format_err!("Unknown string kind {}", kind)),
    }
}

/// Copies data from a PyBytesObject (currently only lnotab object)
pub fn copy_bytes<T: BytesObject, P: ProcessMemory>(
    ptr: *const T,
    process: &P,
) -> Result<Vec<u8>, anyhow::Error> {
    let obj = process.copy_pointer(ptr)?;
    let size = obj.size()?;
    if size >= 65536 {
        return Err(anyhow::format_err!("Refusing to copy {} bytes", size));
    }
    let address = obj.address(ptr as usize)?;
    address
        .checked_add(size)
        .ok_or_else(|| anyhow::format_err!("Byte range overflow"))?;
    Ok(process.copy(address, size)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_each_failed_stack_once() {
        let _guard = crate::signal_sampler::METRICS_LOCK.lock().unwrap();
        let mut resolver = Resolver::new(
            Types {
                code: 1,
                string: 2,
                dict: 3,
            },
            Lines::LastInstruction,
        );
        let before = METRICS.resolve_error.load(Ordering::Relaxed);
        let invalid_offset = Frame {
            code: 1,
            instr: i32::MAX as usize + 2,
        };
        assert!(matches!(
            resolver.resolve_stack(&[invalid_offset; 2]),
            Err(Error::InstructionOffset(_))
        ));
        assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 1);

        // The address range overflows before any memory read or fault recovery.
        assert!(matches!(
            resolver.resolve_stack(&[Frame {
                code: usize::MAX,
                instr: usize::MAX,
            }]),
            Err(Error::PySpy(_))
        ));
        assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 2);

        assert!(resolver.resolve_stack(&[]).unwrap().is_empty());
        assert_eq!(METRICS.resolve_error.load(Ordering::Relaxed), before + 2);
    }

    #[cfg(not(miri))]
    #[test]
    fn reads_with_kindasafe_and_recovers_after_faults() {
        // SAFETY: tests preserve kindasafe's handlers and do not install other handlers.
        unsafe { kindasafe_init::init() }.unwrap();
        let memory = Memory;
        assert!(memory.copy_struct::<usize>(0).is_err());
        assert!(memory.copy(0, 4).is_err());
        assert!(memory.copy(usize::MAX - 1, 4).is_err());
        assert!(memory.copy(0, 0).unwrap().is_empty());

        let word = 0x0123_4567_89ab_cdefusize;
        assert_eq!(
            memory
                .copy_struct::<usize>(&raw const word as usize)
                .unwrap(),
            word
        );
        let bytes = [11u8, 22, 33, 44];
        assert_eq!(
            memory.copy(bytes.as_ptr() as usize, bytes.len()).unwrap(),
            bytes
        );
    }

    #[test]
    fn converts_recorded_instruction_to_pyspy_offset() {
        #[cfg(Py_3_11)]
        {
            let frame = Frame {
                code: 4096,
                instr: 4300,
            };
            assert_eq!(instruction_offset(&frame).unwrap(), 204);
            assert!(
                instruction_offset(&Frame {
                    code: 4096,
                    instr: 4095
                })
                .is_err()
            );
            assert!(
                instruction_offset(&Frame {
                    code: 0,
                    instr: usize::MAX
                })
                .is_err()
            );
        }
        #[cfg(not(Py_3_11))]
        {
            assert_eq!(
                instruction_offset(&Frame {
                    code: 4096,
                    instr: 12
                })
                .unwrap(),
                12
            );
            assert_eq!(
                instruction_offset(&Frame {
                    code: 4096,
                    instr: usize::MAX
                })
                .unwrap(),
                -1
            );
        }
    }
}
