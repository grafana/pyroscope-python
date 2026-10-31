//! Resolves the code objects a signal handler recorded into names, from a normal thread of the
//! same process. A recorded code object may have been freed since, so every read goes through
//! `kindasafe` and the type of every string and line table is checked before it is used.

use pyroscope_python_offsets::resolve as layout;

use super::unwind::{Frame, OB_TYPE};
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
        let filename = self.read_string(read_word(code + CO_FILENAME)?)?;
        if filename.is_empty() || filename == "<shim>" {
            return Ok(None);
        }
        let name = self.read_string(read_word(code + CO_QUALNAME)?)?;
        let line = match self.lines {
            Lines::None => 0,
            Lines::First => first_line(code)?,
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
        let first_line = first_line(frame.code)?;
        let table = read_word(frame.code + CO_LINETABLE)?;
        if table == 0 || read_word(table + OB_TYPE)? != self.types.bytes {
            return Err(format!("{table:#x} is not an exact bytes"));
        }
        let size = read_word(table + BYTES_SIZE)?;
        if size > MAX_LINETABLE_SIZE {
            return Err(format!("line table at {table:#x} is too long: {size}"));
        }
        self.read_bytes(table + BYTES_DATA, size)?;
        let lasti = frame.instr.wrapping_sub(frame.code + CO_CODE_ADAPTIVE) as isize;
        Ok(line_number(first_line as i32, lasti, &self.scratch).max(0) as u32)
    }

    fn read_string(&mut self, addr: usize) -> Result<String, String> {
        if addr == 0 {
            return Err("null string".to_string());
        }
        if read_word(addr + OB_TYPE)? != self.types.unicode {
            return Err(format!("{addr:#x} is not an exact str"));
        }
        let length = read_word(addr + UNICODE_LENGTH)?;
        if length > MAX_STRING_LENGTH {
            return Err(format!("string at {addr:#x} is too long: {length}"));
        }
        let state = read_word(addr + UNICODE_STATE)? as u64;
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
        kindasafe::slice(&mut self.scratch, addr as u64)
            .map_err(|e| format!("failed to read {len} bytes at {addr:#x}: {e:?}"))
    }
}

fn first_line(code: usize) -> Result<u32, String> {
    // `co_firstlineno` is an `int`.
    Ok(read_word(code + CO_FIRSTLINENO)? as u32)
}

/// Unpacks the location table of python 3.11+, see
/// <https://github.com/python/cpython/blob/3.11/Objects/locations.md>. `lasti` is the byte offset
/// of the instruction from `co_code_adaptive`. Ported from py-spy.
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

fn read_signed_varint(index: &mut usize, table: &[u8]) -> Option<isize> {
    let unsigned_val = read_varint(index, table)?;
    if unsigned_val & 1 != 0 {
        Some(-((unsigned_val >> 1) as isize))
    } else {
        Some((unsigned_val >> 1) as isize)
    }
}

fn read_word(addr: usize) -> Result<usize, String> {
    kindasafe::u64(addr as u64)
        .map(|word| word as usize)
        .map_err(|e| format!("failed to read {addr:#x}: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
