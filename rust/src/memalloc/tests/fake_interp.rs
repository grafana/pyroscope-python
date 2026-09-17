//! A synthetic CPython object graph.
//!
//! The frame walker is the riskiest part of the profiler: it reads CPython
//! internals through offsets, inside an allocator hook. It also cannot be
//! tested against a real interpreter from `cargo test`, because the test
//! harness has no libpython and pyo3's `auto-initialize` is not enabled.
//!
//! So this builds a fake one. Frames, code objects, strings and line tables
//! are laid out in a single byte arena with the same shapes CPython uses, and
//! [`FakeInterp`] reports offsets that describe its own layout. The walker
//! then runs against it unmodified, which exercises all of the pointer
//! arithmetic, the owner filter, the tag masking, the string decoding and the
//! frame caps with no interpreter in sight.
//!
//! Everything lives in one `Vec<u8>` so that under Miri every address the
//! walker touches has valid provenance, which turns the whole walk into a
//! checked operation.

// Test scaffolding, never on the hook path, so the panic wall does not apply.
// Panicking here is the point: an out-of-range arena write is a bug in a test,
// and a loud failure is what should happen.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use crate::memalloc::pure::frames::{Mem, TypeAddrs};
use crate::memalloc::pure::offsets::Offsets;

/// Layout of the fake objects. Chosen to look like real CPython without
/// copying any particular version, so the walker cannot accidentally depend on
/// the real numbers.
mod layout {
    // PyObject header: refcount then type pointer.
    pub const OB_TYPE: usize = 8;
    pub const OBJECT_HEADER: usize = 16;

    // Fake PyASCIIObject: header, length, hash, state.
    pub const UNICODE_LENGTH: usize = OBJECT_HEADER;
    pub const UNICODE_STATE: usize = OBJECT_HEADER + 16;
    pub const UNICODE_DATA: usize = OBJECT_HEADER + 24;
    pub const UNICODE_SIZE: usize = UNICODE_DATA;

    // Fake PyBytesObject: header, ob_size, hash, then inline bytes.
    pub const BYTES_OB_SIZE: usize = OBJECT_HEADER;
    pub const BYTES_OB_SVAL: usize = OBJECT_HEADER + 16;
    pub const BYTES_SIZE: usize = BYTES_OB_SVAL;

    // Fake PyCodeObject.
    pub const CODE_FIRSTLINENO: usize = OBJECT_HEADER;
    pub const CODE_FILENAME: usize = OBJECT_HEADER + 8;
    pub const CODE_NAME: usize = OBJECT_HEADER + 16;
    pub const CODE_QUALNAME: usize = OBJECT_HEADER + 24;
    pub const CODE_LINETABLE: usize = OBJECT_HEADER + 32;
    pub const CODE_CO_CODE_ADAPTIVE: usize = OBJECT_HEADER + 40;
    /// Room for inline bytecode after the header.
    pub const CODE_SIZE: usize = CODE_CO_CODE_ADAPTIVE + 256;

    // Fake _PyInterpreterFrame.
    pub const FRAME_EXECUTABLE: usize = 0;
    pub const FRAME_PREVIOUS: usize = 8;
    pub const FRAME_INSTR_PTR: usize = 16;
    pub const FRAME_OWNER: usize = 24;
    pub const FRAME_SIZE: usize = 32;

    // Fake PyThreadState.
    pub const TSTATE_CURRENT_FRAME: usize = 8;
    pub const TSTATE_SIZE: usize = 64;

    // Fake PyTypeObject, only ever compared by address.
    pub const TYPE_TP_FLAGS: usize = 8;
    pub const TYPE_SIZE: usize = 64;
}

pub const FRAME_OWNED_BY_THREAD: u8 = 0;
pub const FRAME_OWNED_BY_GENERATOR: u8 = 1;
pub const FRAME_OWNED_BY_FRAME_OBJECT: u8 = 2;
pub const FRAME_OWNED_BY_CSTACK: u8 = 3;

/// A byte arena laid out like a CPython heap.
pub struct FakeInterp {
    arena: Vec<u8>,
    next: usize,
    code_type: usize,
    unicode_type: usize,
}

impl FakeInterp {
    /// Create an arena with room for `capacity` bytes.
    ///
    /// The arena never grows: addresses handed out are offsets into it, so a
    /// reallocation would invalidate them. Allocation past the end panics, in
    /// a test, which is the right failure.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut interp = Self {
            arena: vec![0u8; capacity],
            // Leave the first word unused so that address 0 is never handed
            // out: the walker treats 0 as null.
            next: 16,
            code_type: 0,
            unicode_type: 0,
        };
        interp.code_type = interp.alloc(layout::TYPE_SIZE);
        interp.unicode_type = interp.alloc(layout::TYPE_SIZE);
        interp
    }

    /// Addresses of the fake type objects.
    pub fn types(&self) -> TypeAddrs {
        TypeAddrs {
            code: self.code_type,
            unicode: self.unicode_type,
        }
    }

    /// Offsets describing this arena's own layout.
    pub fn offsets(&self) -> Offsets {
        Offsets {
            thread_state_size: layout::TSTATE_SIZE,
            thread_state_current_frame: layout::TSTATE_CURRENT_FRAME,
            frame_size: layout::FRAME_SIZE,
            frame_previous: layout::FRAME_PREVIOUS,
            frame_executable: layout::FRAME_EXECUTABLE,
            frame_instr_ptr: layout::FRAME_INSTR_PTR,
            frame_owner: layout::FRAME_OWNER,
            code_size: layout::CODE_SIZE,
            code_filename: layout::CODE_FILENAME,
            code_name: layout::CODE_NAME,
            code_qualname: layout::CODE_QUALNAME,
            code_linetable: layout::CODE_LINETABLE,
            code_firstlineno: layout::CODE_FIRSTLINENO,
            code_co_code_adaptive: layout::CODE_CO_CODE_ADAPTIVE,
            bytes_ob_size: layout::BYTES_OB_SIZE,
            bytes_ob_sval: layout::BYTES_OB_SVAL,
            unicode_size: layout::UNICODE_SIZE,
            unicode_state: layout::UNICODE_STATE,
            unicode_length: layout::UNICODE_LENGTH,
            unicode_asciiobject_size: layout::UNICODE_DATA,
            pyobject_ob_type: layout::OB_TYPE,
            type_tp_flags: layout::TYPE_TP_FLAGS,
        }
    }

    fn alloc(&mut self, size: usize) -> usize {
        // 8-align every object, as any real allocator would.
        let addr = (self.next + 7) & !7;
        let end = addr + size;
        assert!(
            end <= self.arena.len(),
            "fake arena exhausted: needed {end} of {} bytes",
            self.arena.len()
        );
        self.next = end;
        addr
    }

    fn write_usize(&mut self, addr: usize, value: usize) {
        self.arena[addr..addr + 8].copy_from_slice(&value.to_ne_bytes());
    }

    fn write_i32(&mut self, addr: usize, value: i32) {
        self.arena[addr..addr + 4].copy_from_slice(&value.to_ne_bytes());
    }

    fn write_u8(&mut self, addr: usize, value: u8) {
        self.arena[addr] = value;
    }

    fn write_bytes(&mut self, addr: usize, value: &[u8]) {
        self.arena[addr..addr + value.len()].copy_from_slice(value);
    }

    /// Build a compact ASCII `str`, the only shape the walker decodes.
    pub fn new_str(&mut self, text: &str) -> usize {
        self.new_str_with_state(text, 0b0110_0000)
    }

    /// Build a `str` with an arbitrary `state` bitfield, for exercising the
    /// non-compact and non-ASCII paths.
    pub fn new_str_with_state(&mut self, text: &str, state: u8) -> usize {
        let addr = self.alloc(layout::UNICODE_SIZE + text.len() + 1);
        let unicode_type = self.unicode_type;
        self.write_usize(addr + layout::OB_TYPE, unicode_type);
        self.write_usize(addr + layout::UNICODE_LENGTH, text.len());
        self.write_u8(addr + layout::UNICODE_STATE, state);
        self.write_bytes(addr + layout::UNICODE_DATA, text.as_bytes());
        addr
    }

    /// Build an object whose type is neither `str` nor `code`.
    pub fn new_foreign_object(&mut self) -> usize {
        let addr = self.alloc(layout::OBJECT_HEADER);
        self.write_usize(addr + layout::OB_TYPE, 0xdead_beef);
        addr
    }

    /// Build a `bytes` object holding `data`.
    pub fn new_bytes(&mut self, data: &[u8]) -> usize {
        let addr = self.alloc(layout::BYTES_SIZE + data.len() + 1);
        self.write_usize(addr + layout::BYTES_OB_SIZE, data.len());
        self.write_bytes(addr + layout::BYTES_OB_SVAL, data);
        addr
    }

    /// Build a `bytes` object that claims an absurd length.
    pub fn new_bytes_claiming_len(&mut self, claimed: usize) -> usize {
        let addr = self.alloc(layout::BYTES_SIZE + 8);
        self.write_usize(addr + layout::BYTES_OB_SIZE, claimed);
        addr
    }

    /// Build a code object.
    pub fn new_code(
        &mut self,
        qualname: &str,
        filename: &str,
        linetable: &[u8],
        firstlineno: i32,
    ) -> usize {
        let qualname_obj = self.new_str(qualname);
        let filename_obj = self.new_str(filename);
        let linetable_obj = self.new_bytes(linetable);
        self.new_code_raw(qualname_obj, 0, filename_obj, linetable_obj, firstlineno)
    }

    /// Build a code object from already-created field objects, so tests can
    /// supply null or wrongly-typed ones.
    pub fn new_code_raw(
        &mut self,
        qualname: usize,
        name: usize,
        filename: usize,
        linetable: usize,
        firstlineno: i32,
    ) -> usize {
        let addr = self.alloc(layout::CODE_SIZE);
        let code_type = self.code_type;
        self.write_usize(addr + layout::OB_TYPE, code_type);
        self.write_i32(addr + layout::CODE_FIRSTLINENO, firstlineno);
        self.write_usize(addr + layout::CODE_FILENAME, filename);
        self.write_usize(addr + layout::CODE_NAME, name);
        self.write_usize(addr + layout::CODE_QUALNAME, qualname);
        self.write_usize(addr + layout::CODE_LINETABLE, linetable);
        addr
    }

    /// Build a frame executing at code-unit index `lasti` of `code`.
    pub fn new_frame(&mut self, code: usize, previous: usize, owner: u8, lasti: usize) -> usize {
        let instr_ptr = code + layout::CODE_CO_CODE_ADAPTIVE + lasti * 2;
        self.new_frame_raw(code, previous, owner, instr_ptr)
    }

    /// Build a frame with an arbitrary `executable` word and `instr_ptr`, for
    /// exercising tag masking and out-of-range instruction pointers.
    pub fn new_frame_raw(
        &mut self,
        executable: usize,
        previous: usize,
        owner: u8,
        instr_ptr: usize,
    ) -> usize {
        let addr = self.alloc(layout::FRAME_SIZE);
        self.write_usize(addr + layout::FRAME_EXECUTABLE, executable);
        self.write_usize(addr + layout::FRAME_PREVIOUS, previous);
        self.write_usize(addr + layout::FRAME_INSTR_PTR, instr_ptr);
        self.write_u8(addr + layout::FRAME_OWNER, owner);
        addr
    }

    /// Point an existing frame's `previous` link somewhere else, so a cycle
    /// can be built.
    pub fn set_frame_previous(&mut self, frame: usize, previous: usize) {
        self.write_usize(frame + layout::FRAME_PREVIOUS, previous);
    }

    /// Build a thread state whose innermost frame is `frame`.
    pub fn new_tstate(&mut self, frame: usize) -> usize {
        let addr = self.alloc(layout::TSTATE_SIZE);
        self.write_usize(addr + layout::TSTATE_CURRENT_FRAME, frame);
        addr
    }

    fn slice(&self, addr: usize, len: usize) -> Option<&[u8]> {
        self.arena.get(addr..addr.checked_add(len)?)
    }
}

// SAFETY: every read is bounds-checked against the arena and returns `None`
// rather than reading out of range.
unsafe impl Mem for FakeInterp {
    fn read_usize(&self, addr: usize) -> Option<usize> {
        let bytes: [u8; 8] = self.slice(addr, 8)?.try_into().ok()?;
        Some(usize::from_ne_bytes(bytes))
    }

    fn read_u8(&self, addr: usize) -> Option<u8> {
        self.arena.get(addr).copied()
    }

    fn read_i32(&self, addr: usize) -> Option<i32> {
        let bytes: [u8; 4] = self.slice(addr, 4)?.try_into().ok()?;
        Some(i32::from_ne_bytes(bytes))
    }

    fn read_bytes(&self, addr: usize, len: usize) -> Option<&[u8]> {
        self.slice(addr, len)
    }
}
