//! Resolves line-number modes using the target CPython's co_linetable decoder.
//!
//! Offsets are bytes, ranges are half-open, and -1 means no source line. The decoders
//! consume untrusted bytes, checking truncation and arithmetic.

use super::{Error, Frame, Lines, Types, check_type, integer, layout, word};

// Bound decoder work for reused addresses or torn reads.
const MAX_LINE_TABLE_BYTES: usize = 1 << 20;

#[cfg(any(not(Py_3_11), all(test, not(miri))))]
mod v3_10;
#[cfg(any(Py_3_11, all(test, not(miri))))]
mod v3_11;

#[cfg(not(Py_3_11))]
use v3_10::line_number as decode;
#[cfg(Py_3_11)]
use v3_11::line_number as decode;

struct Table {
    address: usize,
    remaining: usize,
}

impl Table {
    fn new(address: usize, len: usize) -> Result<Self, Error> {
        if len > MAX_LINE_TABLE_BYTES {
            return Err(Error::Length);
        }
        Ok(Self {
            address,
            remaining: len,
        })
    }

    #[inline(always)]
    fn peek(&self) -> Result<Option<u8>, Error> {
        if self.remaining == 0 {
            return Ok(None);
        }
        // SAFETY: the resolver's recovery and source-borrow preconditions apply.
        // Read only a byte within the declared table, without forming a source reference.
        unsafe { kindasafe::u8(self.address as u64) }
            .map(Some)
            .map_err(|_| Error::ReadMemory)
    }

    #[inline(always)]
    fn next(&mut self) -> Result<u8, Error> {
        let byte = self.peek()?.ok_or(Error::LineTable)?;
        self.address = self.address.wrapping_add(1);
        self.remaining -= 1;
        Ok(byte)
    }
}

pub(super) fn resolve<T: Types>(frame: &Frame, mode: Lines) -> Result<i32, Error> {
    Ok(match mode {
        Lines::NoLine => 0,
        Lines::First => integer(frame.code, layout::CO_FIRSTLINENO)?,
        Lines::LastInstruction => {
            let offset = instruction_offset(frame)?;
            let first = integer(frame.code, layout::CO_FIRSTLINENO)?;
            if offset < 0 {
                first
            } else {
                let address = word(frame.code, layout::CO_LINETABLE)?;
                check_type(address, T::bytes())?;
                let len = word(address, layout::BYTES_SIZE)?;
                let table = Table::new(address.wrapping_add(layout::BYTES_DATA), len)?;
                line_number(table, first, offset)?
            }
        }
    })
}

#[inline(always)]
fn instruction_offset(frame: &Frame) -> Result<i32, Error> {
    // CPython's _Py_CODEUNIT is two bytes. 3.10 stores an index; 3.11+ stores a
    // pointer into co_code_adaptive. A frame that has not started can point one unit before it.
    #[cfg(not(Py_3_11))]
    let offset = (frame.instr as isize)
        .checked_mul(2)
        .ok_or(Error::InstructionOffset)?;
    #[cfg(Py_3_11)]
    let offset = {
        let start = frame
            .code
            .checked_add(layout::CO_CODE_ADAPTIVE)
            .ok_or(Error::InstructionOffset)?;
        if frame.instr == start - 2 {
            -2
        } else {
            let offset = frame
                .instr
                .checked_sub(start)
                .ok_or(Error::InstructionOffset)?;
            isize::try_from(offset).map_err(|_| Error::InstructionOffset)?
        }
    };
    if offset < -2 || offset % 2 != 0 {
        return Err(Error::InstructionOffset);
    }
    i32::try_from(offset).map_err(|_| Error::InstructionOffset)
}

#[inline(always)]
fn line_number(table: Table, first: i32, offset: i32) -> Result<i32, Error> {
    if offset < 0 {
        return Ok(first);
    }
    decode(table, first, offset)
}

#[cfg(test)]
mod tests {
    use super::super::tests::recorded_frame;
    use super::*;

    #[test]
    fn converts_instructions_to_byte_offsets() {
        let frame = recorded_frame(4096, 24);
        assert_eq!(instruction_offset(&frame).unwrap(), 24);
        let start = recorded_frame(4096, 0);
        #[cfg(Py_3_11)]
        let before = Frame {
            instr: start.instr - 2,
            ..start
        };
        #[cfg(not(Py_3_11))]
        let before = Frame {
            instr: usize::MAX,
            ..start
        };
        assert_eq!(instruction_offset(&before).unwrap(), -2);
        let too_far = recorded_frame(4096, i32::MAX as usize + 1);
        assert!(instruction_offset(&too_far).is_err());
        #[cfg(Py_3_11)]
        {
            assert!(
                instruction_offset(&Frame {
                    instr: start.instr - 4,
                    ..start
                })
                .is_err()
            );
            assert!(
                instruction_offset(&Frame {
                    instr: start.instr + 1,
                    ..start
                })
                .is_err()
            );
            assert!(
                instruction_offset(&Frame {
                    code: usize::MAX,
                    instr: 0
                })
                .is_err()
            );
        }
    }

    #[cfg(not(miri))]
    #[test]
    fn table_reads_live_bytes_with_length_bounds() {
        super::super::tests::recovery();
        let mut bytes = [11u8, 22];
        let mut table = Table::new(bytes.as_ptr() as usize, bytes.len()).unwrap();
        assert_eq!(table.next().unwrap(), 11);
        bytes[1] = 33;
        assert_eq!(table.next().unwrap(), bytes[1]);
        assert_eq!(table.peek().unwrap(), None);
        assert!(matches!(table.next(), Err(Error::LineTable)));
        let mut empty = Table::new(0, 0).unwrap();
        assert!(matches!(empty.next(), Err(Error::LineTable)));
        assert!(matches!(
            Table::new(0, MAX_LINE_TABLE_BYTES + 1),
            Err(Error::Length)
        ));
    }

    #[cfg(not(miri))]
    #[test]
    fn decodes_readable_prefix_without_copying_unreadable_tail() {
        super::super::tests::recovery();
        let memory = GuardedMemory::new();

        let address = memory.write_prefix(&[2, 0]);
        assert_eq!(
            v3_10::line_number(Table::new(address, 4).unwrap(), 10, 0).unwrap(),
            10
        );
        assert!(matches!(
            v3_10::line_number(Table::new(address, 4).unwrap(), 10, 2),
            Err(Error::ReadMemory)
        ));

        let address = memory.write_prefix(&[0x80, 0x01, 0x80]);
        assert_eq!(
            v3_11::line_number(Table::new(address, 4).unwrap(), 10, 0).unwrap(),
            10
        );
        assert!(matches!(
            v3_11::line_number(Table::new(address, 4).unwrap(), 10, 2),
            Err(Error::ReadMemory)
        ));

        // A complete entry ending at the last readable byte must not overread.
        let address = memory.write_prefix(&[0x80, 0x01]);
        assert_eq!(
            v3_11::line_number(Table::new(address, 2).unwrap(), 10, 0).unwrap(),
            10
        );
    }

    #[cfg(not(miri))]
    struct GuardedMemory {
        address: *mut libc::c_void,
        page_size: usize,
    }

    #[cfg(not(miri))]
    impl GuardedMemory {
        fn new() -> Self {
            // SAFETY: sysconf takes no pointers; the mapping is owned until Drop.
            let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            assert!(page_size > 0);
            let page_size = page_size as usize;
            // SAFETY: request two anonymous private pages with no fixed address.
            let address = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    page_size * 2,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            assert_ne!(address, libc::MAP_FAILED);
            let memory = Self { address, page_size };
            // SAFETY: the second page is aligned and belongs to this mapping.
            assert_eq!(
                unsafe { libc::mprotect(address.byte_add(page_size), page_size, libc::PROT_NONE) },
                0
            );
            memory
        }

        fn write_prefix(&self, bytes: &[u8]) -> usize {
            assert!(bytes.len() <= self.page_size);
            // SAFETY: the destination is within the writable first page and disjoint
            // from the fixture bytes. No source reference survives into decoding.
            unsafe {
                let address = self.address.cast::<u8>().add(self.page_size - bytes.len());
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), address, bytes.len());
                address as usize
            }
        }
    }

    #[cfg(not(miri))]
    impl Drop for GuardedMemory {
        fn drop(&mut self) {
            // SAFETY: this test owns both pages and no decoder uses them after Drop.
            unsafe {
                libc::munmap(self.address, self.page_size * 2);
            }
        }
    }

    #[cfg(not(miri))]
    #[test]
    fn matches_running_cpython_co_lines_for_every_byte() {
        super::super::tests::recovery();
        // Generate the oracle with the target interpreter, never with another decoder.
        let python = std::env::var_os("Python3_EXECUTABLE").expect("target Python interpreter");
        let script = include_str!("../../../../scripts/tests/co_lines.py");
        for no_columns in [false, true] {
            let mut command = std::process::Command::new(&python);
            if no_columns {
                command.args(["-X", "no_debug_ranges"]);
            }
            let output = command.args(["-c", script]).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout).unwrap();
            let mut count = 0;
            for fixture in stdout.lines() {
                let fields: Vec<_> = fixture.split('|').collect();
                let first = fields[0].parse().unwrap();
                let table: Vec<_> = fields[1]
                    .as_bytes()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                for (offset, expected) in fields[2].split(',').enumerate() {
                    assert_eq!(
                        line_number(
                            Table::new(table.as_ptr() as usize, table.len()).unwrap(),
                            first,
                            offset as i32
                        )
                        .unwrap(),
                        expected.parse::<i32>().unwrap(),
                        "CPython {:?}, no_columns={no_columns}, offset={offset}, fixture={fixture}",
                        pyroscope_python_unwind::PY_VERSION
                    );
                    count += 1;
                }
            }
            assert!(
                count > 1000,
                "oracle must exercise substantial bytecode: {count}"
            );
        }
    }
}
