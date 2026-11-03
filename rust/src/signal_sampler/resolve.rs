//! Resolves the code objects a signal handler recorded into names, from a normal thread of the
//! same process. Kindasafe reads potentially stale addresses into untrusted bytes. String and
//! line-table types and lengths are checked before use; torn reads or a reused address can
//! still produce a misattributed sample.
//!
//! Session startup initializes recovery. The application must preserve its handlers and keep
//! SIGSEGV/SIGBUS unblocked on the resolver thread. Source ranges must not overlap unrelated
//! live Rust exclusive borrows, including after allocation reuse. These are runtime assumptions,
//! not properties established by a recorded address or by fault recovery.

use pyroscope_python_offsets as layout;

use super::unwind::Frame;
use super::{Lines, ResolvedFrame};
use layout::*;

const MAX_STRING_LENGTH: usize = 4096;
const MAX_LINETABLE_SIZE: usize = 1 << 20;

pub struct Types {
    pub unicode: usize,
    pub bytes: usize,
}

pub struct Resolver {
    types: Types,
    lines: Lines,
    scratch: Vec<u8>,
}

impl Resolver {
    pub fn new(types: Types, lines: Lines) -> Self {
        Resolver {
            types,
            lines,
            scratch: Vec::new(),
        }
    }

    pub fn resolve_stack(&mut self, frames: &[Frame]) -> Result<Vec<ResolvedFrame>, String> {
        let mut resolved = Vec::with_capacity(frames.len());
        for frame in frames {
            if let Some(frame) = self.resolve_frame(frame)? {
                resolved.push(frame);
            }
        }
        Ok(resolved)
    }

    /// `None` for shim frames, which have no file name.
    fn resolve_frame(&mut self, frame: &Frame) -> Result<Option<ResolvedFrame>, String> {
        let code = frame.code;
        let filename = self.read_string(self.read_word(code + CO_FILENAME)?)?;
        if filename.is_empty() || filename == "<shim>" {
            return Ok(None);
        }
        let name = self.read_string(self.read_word(code + CO_QUALNAME)?)?;
        let line = match self.lines {
            Lines::None => 0,
            Lines::First => self.first_line(code)?,
            Lines::LastInstruction => self.line(frame).unwrap_or(0),
        };
        Ok(Some(ResolvedFrame {
            name,
            filename,
            line,
        }))
    }

    /// A line number that cannot be read is not worth dropping the stack for.
    fn line(&mut self, frame: &Frame) -> Result<u32, String> {
        let first_line = self.first_line(frame.code)?;
        let table = self.read_word(frame.code + CO_LINETABLE)?;
        if table == 0 || self.read_word(table + OB_TYPE)? != self.types.bytes {
            return Err(format!("{table:#x} is not an exact bytes"));
        }
        let size = self.read_word(table + BYTES_SIZE)?;
        if size > MAX_LINETABLE_SIZE {
            return Err(format!("line table at {table:#x} is too long: {size}"));
        }
        self.read_bytes(table + BYTES_DATA, size)?;
        #[cfg(Py_3_11)]
        let line = {
            let lasti = frame.instr.wrapping_sub(frame.code + CO_CODE_ADAPTIVE) as isize;
            line_number(first_line as i32, lasti, &self.scratch)
        };
        #[cfg(not(Py_3_11))]
        let line = line_number_310(first_line as i32, frame.instr as isize, &self.scratch);
        Ok(line.max(0) as u32)
    }

    fn read_string(&mut self, addr: usize) -> Result<String, String> {
        if addr == 0 {
            return Err("null string".to_string());
        }
        if self.read_word(addr + OB_TYPE)? != self.types.unicode {
            return Err(format!("{addr:#x} is not an exact str"));
        }
        let length = self.read_word(addr + UNICODE_LENGTH)?;
        if length > MAX_STRING_LENGTH {
            return Err(format!("string at {addr:#x} is too long: {length}"));
        }
        let state = self.read_word(addr + UNICODE_STATE)? as u64;
        #[cfg(not(Py_3_11))]
        if state & STATE_READY == 0 {
            return Err(format!("string at {addr:#x} is not ready"));
        }
        if state & STATE_COMPACT == 0 {
            return Err(format!("string at {addr:#x} is not compact"));
        }
        if state & STATE_ASCII != 0 {
            self.read_bytes(addr + ASCII_SIZE, length)?;
            return String::from_utf8(self.scratch.clone())
                .map_err(|_| format!("ascii string at {addr:#x} is not utf-8"));
        }
        let kind = ((state & STATE_KIND_MASK) >> STATE_KIND_SHIFT) as usize;
        if !matches!(kind, 1 | 2 | 4) {
            return Err(format!("string at {addr:#x} has kind {kind}"));
        }
        self.read_bytes(addr + COMPACT_SIZE, length * kind)?;
        let units = self.scratch.chunks_exact(kind).map(|unit| match kind {
            1 => unit[0] as u32,
            2 => u16::from_ne_bytes([unit[0], unit[1]]) as u32,
            _ => u32::from_ne_bytes([unit[0], unit[1], unit[2], unit[3]]),
        });
        Ok(units
            .map(|unit| char::from_u32(unit).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect())
    }

    fn read_bytes(&mut self, addr: usize, len: usize) -> Result<(), String> {
        self.scratch.clear();
        self.scratch.resize(len, 0);
        read_into(addr, &mut self.scratch)
    }

    fn first_line(&self, code: usize) -> Result<u32, String> {
        // `co_firstlineno` is an `int`.
        let mut out = [0; 4];
        let addr = code
            .checked_add(CO_FIRSTLINENO)
            .ok_or("memory address range overflow")?;
        read_into(addr, &mut out)?;
        Ok(u32::from_ne_bytes(out))
    }

    fn read_word(&self, addr: usize) -> Result<usize, String> {
        addr.checked_add(size_of::<usize>())
            .ok_or("memory address range overflow")?;
        // SAFETY: recovery and source-borrow assumptions are documented at module level.
        // Supported targets are 64-bit; consume the result as untrusted bytes.
        unsafe { kindasafe::u64(addr as u64) }
            .map(|word| word as usize)
            .map_err(|e| format!("failed to read {addr:#x}: {e:?}"))
    }
}

/// An error may leave a partial copy in `out`; callers must discard it.
fn read_into(addr: usize, out: &mut [u8]) -> Result<(), String> {
    addr.checked_add(out.len())
        .ok_or("memory address range overflow")?;
    if out.is_empty() {
        return Ok(());
    }
    // SAFETY: recovery and source-borrow assumptions are documented at module level.
    // The destination is separate, initialized, exclusively borrowed storage. The source
    // range is checked for overflow and copied without constructing a source reference.
    unsafe { kindasafe::slice(out, addr as u64) }
        .map_err(|e| format!("failed to read {} bytes at {addr:#x}: {e:?}", out.len()))
}

/// Unpacks the location table of python 3.11+, see
/// <https://github.com/python/cpython/blob/3.11/Objects/locations.md>. `lasti` is the byte offset
/// of the instruction from `co_code_adaptive`. Ported from py-spy.
#[cfg(any(test, Py_3_11))]
fn line_number(first_line: i32, lasti: isize, table: &[u8]) -> i32 {
    let mut line_number = first_line;
    let mut bytecode_address: isize = 0;
    let mut index: usize = 0;
    while index < table.len() {
        let byte = table[index];
        index += 1;

        let delta = ((byte & 7) as isize) + 1;
        bytecode_address += delta * 2;
        let code = (byte >> 3) & 15;
        let line_delta = match code {
            15 => 0,
            14 => {
                let delta = read_signed_varint(&mut index, table).unwrap_or(0);
                read_varint(&mut index, table); // end line
                read_varint(&mut index, table); // start column
                read_varint(&mut index, table); // end column
                delta
            }
            13 => read_signed_varint(&mut index, table).unwrap_or(0),
            10..=12 => {
                index += 2; // start column / end column
                (code - 10).into()
            }
            _ => {
                index += 1; // column
                0
            }
        };
        line_number += line_delta as i32;
        if bytecode_address > lasti {
            break;
        }
    }
    line_number
}

/// Unpacks 3.10's (byte range length, signed line delta) pairs. `lasti` counts code units.
/// <https://github.com/python/cpython/blob/3.10/Objects/lnotab_notes.txt>
#[cfg(any(test, not(Py_3_11)))]
fn line_number_310(first_line: i32, lasti: isize, table: &[u8]) -> i32 {
    if lasti < 0 {
        return first_line;
    }
    let mut line = first_line;
    let mut end = 0;
    for entry in table.as_chunks::<2>().0 {
        end += isize::from(entry[0]);
        let delta = entry[1] as i8;
        if delta != -128 {
            line += i32::from(delta);
        }
        if end > lasti * 2 {
            return if delta == -128 { 0 } else { line };
        }
    }
    0
}

#[cfg(any(test, Py_3_11))]
fn read_varint(index: &mut usize, table: &[u8]) -> Option<usize> {
    let mut byte = *table.get(*index)?;
    *index += 1;
    let mut ret = (byte & 63) as usize;
    let mut shift = 0;
    while byte & 64 != 0 {
        byte = *table.get(*index)?;
        *index += 1;
        shift += 6;
        ret += ((byte & 63) as usize).checked_shl(shift)?;
    }
    Some(ret)
}

#[cfg(any(test, Py_3_11))]
fn read_signed_varint(index: &mut usize, table: &[u8]) -> Option<isize> {
    let unsigned_val = read_varint(index, table)?;
    if unsigned_val & 1 != 0 {
        Some(-((unsigned_val >> 1) as isize))
    } else {
        Some((unsigned_val >> 1) as isize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(miri))]
    fn resolver() -> Resolver {
        super::super::init().unwrap();
        Resolver::new(
            Types {
                unicode: 0,
                bytes: 0,
            },
            Lines::First,
        )
    }

    #[cfg(not(miri))]
    #[test]
    fn reads_fields_and_bytes_with_kindasafe() {
        let mut resolver = resolver();
        let word = 0x0123_4567_89ab_cdefusize;
        assert_eq!(resolver.read_word(&raw const word as usize).unwrap(), word);

        let bytes = [11u8, 22, 33, 44];
        resolver
            .read_bytes(bytes.as_ptr() as usize, bytes.len())
            .unwrap();
        assert_eq!(resolver.scratch, bytes);

        // co_firstlineno has exactly four bytes, not a pointer-sized field.
        let mut code = [0u8; CO_FIRSTLINENO + 4];
        code[CO_FIRSTLINENO..].copy_from_slice(&123u32.to_ne_bytes());
        assert_eq!(resolver.first_line(code.as_ptr() as usize).unwrap(), 123);
    }

    #[cfg(not(miri))]
    #[test]
    fn failed_reads_return_errors_and_allow_later_reads() {
        let mut resolver = resolver();
        assert!(resolver.read_word(0).is_err());
        assert!(resolver.read_bytes(0, 4).is_err());
        assert!(resolver.first_line(0).is_err());
        assert!(resolver.read_word(usize::MAX - 1).is_err());
        assert!(resolver.read_bytes(usize::MAX - 1, 4).is_err());
        assert!(resolver.first_line(usize::MAX).is_err());

        resolver.read_bytes(0, 0).unwrap();
        assert!(resolver.scratch.is_empty());
        let bytes = [55u8, 66];
        resolver
            .read_bytes(bytes.as_ptr() as usize, bytes.len())
            .unwrap();
        assert_eq!(resolver.scratch, bytes);
    }

    #[test]
    fn python_310_line_number_ranges() {
        // CPython's documented example: long ranges, no line, and an empty range
        // used to encode a large positive delta, followed by a backwards line jump.
        let table = [
            6, 1, 44, 1, 254, 5, 46, 0, 10, 128, 16, 1, 0, 127, 4, 73, 2, 255,
        ];
        for (start, end, line) in [
            (0, 6, 1),
            (6, 50, 2),
            (50, 350, 7),
            (350, 360, 0),
            (360, 376, 8),
            (376, 380, 208),
            (380, 382, 207),
        ] {
            for byte in (start..end).step_by(2) {
                assert_eq!(line_number_310(0, byte / 2, &table), line, "byte {byte}");
            }
        }
        assert_eq!(line_number_310(12, -1, &table), 12);
        assert_eq!(line_number_310(12, 0, &[]), 0);
        assert_eq!(line_number_310(12, 0, &[2]), 0);
    }

    #[test]
    fn test_py3_11_line_numbers() {
        let table = [
            128_u8, 0, 221, 4, 8, 132, 74, 136, 118, 209, 4, 22, 212, 4, 22, 208, 4, 22, 208, 4,
            22, 208, 4, 22,
        ];
        // `co_code_adaptive` is at 184 on 3.11.
        let lasti = 214 - 184;
        assert_eq!(line_number(4, lasti, &table), 5);
    }

    #[test]
    fn test_compact_line_number_boundaries() {
        let table = [
            240, 3, 1, 1, 1, 242, 2, 3, 1, 17, 240, 12, 0, 7, 11, 217, 4, 11, 136, 66, 134, 75,
        ];
        for (start, end, line) in [(0, 2, 0), (2, 8, 1), (8, 10, 7), (10, 30, 8)] {
            for offset in (start..end).step_by(2) {
                assert_eq!(
                    line_number(1, offset, &table),
                    line,
                    "incorrect line for bytecode offset {}",
                    offset
                );
            }
        }
    }
}
