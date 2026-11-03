//! Resolves the code objects a signal handler recorded into names, from a normal thread of the
//! same process. Kindasafe reads potentially stale addresses into untrusted bytes. Py-spy
//! validates code and string types and bounds copied lengths, then decodes names and lines.
//! Torn reads or a reused address can still produce a misattributed sample.
//!
//! Session startup initializes recovery. The application must preserve its handlers and keep
//! SIGSEGV/SIGBUS unblocked on the resolver thread. Source ranges must not overlap unrelated
//! live Rust exclusive borrows, including after allocation reuse. These are runtime assumptions,
//! not properties established by a recorded address or by fault recovery.

use py_spy::{
    python_bindings as bindings,
    python_process_info::{TypeChecks, Types},
    stack_trace::{RawFrame, resolve_frames},
};
use remoteprocess::{Error, ProcessMemory};

use super::unwind::Frame;
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

pub struct Resolver {
    types: TypeChecks,
    lines: Lines,
    raw: Vec<RawFrame>,
}

impl Resolver {
    pub fn new(types: Types, lines: Lines) -> Self {
        Self {
            types: TypeChecks::Strict(types),
            lines,
            raw: Vec::new(),
        }
    }

    pub fn resolve_stack(&mut self, frames: &[Frame]) -> Result<Vec<ResolvedFrame>, String> {
        self.raw.clear();
        for frame in frames {
            let lasti = match self.lines {
                Lines::LastInstruction => instruction_offset(frame)?,
                Lines::First | Lines::NoLine => 0,
            };
            self.raw.push(RawFrame::from_code(frame.code, lasti));
        }
        resolve_frames::<PythonFrame, _>(&self.raw, &Memory, false, self.lines, &self.types)
            .map_err(|error| format!("{error:#}"))
    }
}

fn instruction_offset(frame: &Frame) -> Result<i32, String> {
    // py-spy expects f_lasti code units on 3.10 and a byte offset from the
    // beginning of the code object on 3.11+, including its header.
    #[cfg(not(Py_3_11))]
    let offset = i32::try_from(frame.instr as isize);
    #[cfg(Py_3_11)]
    let offset = i32::try_from(
        frame
            .instr
            .checked_sub(frame.code)
            .ok_or("instruction precedes code object")?,
    );
    offset.map_err(|_| "instruction offset is out of range".to_string())
}

/// Supplies py-spy's decoder with fault-recovering reads of this process.
struct Memory;

impl ProcessMemory for Memory {
    fn read(&self, addr: usize, out: &mut [u8]) -> Result<(), Error> {
        addr.checked_add(out.len())
            .ok_or_else(|| Error::Other("memory address range overflow".to_string()))?;
        if out.is_empty() {
            return Ok(());
        }
        // SAFETY: recovery and source-borrow assumptions are documented at module level.
        // The destination is separate, initialized, exclusively borrowed storage. The source
        // range is checked for overflow and copied without constructing a source reference.
        unsafe { kindasafe::slice(out, addr as u64) }.map_err(|error| {
            Error::Other(format!(
                "failed to read {} bytes at {addr:#x}: {error:?}",
                out.len()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(miri))]
    #[test]
    fn reads_with_kindasafe_and_recovers_after_faults() {
        super::super::init().unwrap();
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
