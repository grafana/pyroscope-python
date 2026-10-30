//! Resolves the code objects a signal handler recorded into names, from a normal thread of the
//! same process. A recorded code object may have been freed since, so every read goes through
//! `kindasafe` and every object's type is checked before it is used.

use super::unwind::{Frame, OB_TYPE};

/// Offsets of one CPython minor version, default (GIL) build, 64-bit. Taken from py-spy's
/// bindings.
pub struct Layout {
    co_filename: usize,
    co_qualname: usize,
    co_firstlineno: usize,
    ascii_size: usize,
    compact_size: usize,
}

const UNICODE_LENGTH: usize = 16;
const UNICODE_STATE: usize = 32;
const STATE_KIND_MASK: u64 = 0x1c;
const STATE_KIND_SHIFT: u64 = 2;
const STATE_COMPACT: u64 = 0x20;
const STATE_ASCII: u64 = 0x40;
const MAX_STRING_LENGTH: usize = 4096;

pub static V3_11: Layout = Layout {
    co_filename: 112,
    co_qualname: 128,
    co_firstlineno: 72,
    ascii_size: 48,
    compact_size: 72,
};

/// Also the layout of 3.13 and 3.14.
pub static V3_12: Layout = Layout {
    co_filename: 112,
    co_qualname: 128,
    co_firstlineno: 68,
    ascii_size: 40,
    compact_size: 56,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFrame {
    pub name: String,
    pub filename: String,
    pub first_line: u32,
}

pub struct Resolver {
    layout: &'static Layout,
    code_type: usize,
    unicode_type: usize,
    scratch: Vec<u8>,
}

impl Resolver {
    pub fn new(layout: &'static Layout, code_type: usize, unicode_type: usize) -> Self {
        Resolver {
            layout,
            code_type,
            unicode_type,
            scratch: Vec::new(),
        }
    }

    pub fn resolve_stack(&mut self, frames: &[Frame]) -> Result<Vec<ResolvedFrame>, String> {
        let mut resolved = Vec::with_capacity(frames.len());
        for frame in frames {
            if let Some(frame) = self.resolve_frame(frame.code)? {
                resolved.push(frame);
            }
        }
        Ok(resolved)
    }

    /// `None` for shim frames, which have no file name.
    fn resolve_frame(&mut self, code: usize) -> Result<Option<ResolvedFrame>, String> {
        if read_word(code + OB_TYPE)? != self.code_type {
            return Err(format!("{code:#x} is no longer a code object"));
        }
        let layout = self.layout;
        let filename = self.read_string(read_word(code + layout.co_filename)?)?;
        if filename.is_empty() || filename == "<shim>" {
            return Ok(None);
        }
        let name = self.read_string(read_word(code + layout.co_qualname)?)?;
        let first_line = read_word(code + layout.co_firstlineno)? as u32;
        Ok(Some(ResolvedFrame {
            name,
            filename,
            first_line,
        }))
    }

    fn read_string(&mut self, addr: usize) -> Result<String, String> {
        if addr == 0 {
            return Err("null string".to_string());
        }
        if read_word(addr + OB_TYPE)? != self.unicode_type {
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
            self.read_bytes(addr + self.layout.ascii_size, length)?;
            return String::from_utf8(self.scratch.clone())
                .map_err(|_| format!("ascii string at {addr:#x} is not utf-8"));
        }
        let kind = ((state & STATE_KIND_MASK) >> STATE_KIND_SHIFT) as usize;
        if !matches!(kind, 1 | 2 | 4) {
            return Err(format!("string at {addr:#x} has kind {kind}"));
        }
        self.read_bytes(addr + self.layout.compact_size, length * kind)?;
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

/// `co_firstlineno` is an `int`, so callers truncate.
fn read_word(addr: usize) -> Result<usize, String> {
    kindasafe::u64(addr as u64)
        .map(|word| word as usize)
        .map_err(|e| format!("failed to read {addr:#x}: {e:?}"))
}
