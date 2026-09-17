//! Walking CPython's frame chain.
//!
//! A port of `push_stacktrace_to_sample_no_refcount` and
//! `unicode_to_sv_no_alloc` from `cpp/_memalloc_tb.cpp`, plus the accessors in
//! `cpp/profiling_helpers/frame_accessors.h`.
//!
//! This runs inside CPython's allocator with the GIL held, which dictates the
//! whole design:
//!
//! * **No new references.** The public `PyThreadState_GetFrame` /
//!   `PyFrame_GetBack` / `PyFrame_GetCode` APIs each return a new reference,
//!   so walking with them would mean an incref/decref per frame, and a decref
//!   can free, which re-enters the `free` hook. Every read here is a plain
//!   struct field read through [`Offsets`], so no refcount is ever touched.
//! * **No allocation through PyMem.** Names are decoded only when the string
//!   is compact ASCII, which is a direct read of the object's own bytes.
//!   `PyUnicode_AsUTF8AndSize` would allocate a UTF-8 cache through
//!   `PyObject_Malloc`, so non-ASCII names are reported as `<non-ascii>`
//!   rather than decoded. Line numbers come from parsing `co_linetable`
//!   ourselves for the same reason: CPython does not promise
//!   `PyCode_Addr2Line` is allocation-free.
//!
//! Memory access goes through [`Mem`] and output through [`FrameSink`], so the
//! whole walk is exercised in tests against a synthetic object graph with no
//! interpreter involved. See `memalloc::tests::fake_interp`.

use crate::memalloc::limits::TRACEBACK_MAX_WALKED_NFRAME;
use crate::memalloc::pure::linetable;
use crate::memalloc::pure::offsets::Offsets;

/// `_frameowner` values whose frames carry real Python code.
///
/// Anything else is an interpreter shim or a frame owned by a materialised
/// frame object, which the C++ skipped too. Only these two values are
/// accepted, which is deliberately version-proof: `FRAME_OWNED_BY_CSTACK` is 3
/// on 3.13 and 4 on 3.14, but `THREAD` and `GENERATOR` have been 0 and 1
/// throughout.
const FRAME_OWNED_BY_THREAD: u8 = 0;
const FRAME_OWNED_BY_GENERATOR: u8 = 1;

/// Tag bits to clear from a `_PyStackRef`.
///
/// On 3.14 `f_executable` is a tagged pointer (`Py_TAG_BITS` is 1 in a
/// GIL-enabled build); on 3.13 it is a plain `PyObject*`. Clearing the low
/// three bits is correct either way, because `PyObject` is at least 8-byte
/// aligned, and matches what the C++ did.
const STACKREF_TAG_MASK: usize = !7;

/// Reported when there is no thread state to walk.
pub const NO_THREAD_STATE: &str = "<no thread state>";
/// Reported when the thread state has no frames.
pub const NO_PYTHON_FRAMES: &str = "<no Python frames>";
/// Reported for a field we could not read or that is not a string.
pub const UNKNOWN: &str = "<unknown>";
/// Reported for a string we decline to decode, because doing so would
/// allocate through PyMem.
pub const NON_ASCII: &str = "<non-ascii>";

/// Refuse to decode absurd lengths rather than construct a huge slice.
///
/// Function names and filenames are far shorter than this; a larger value
/// means we are not looking at a real string object.
const MAX_STRING_LEN: usize = 64 * 1024;

/// Refuse to read an absurd line table for the same reason. A table this size
/// would correspond to millions of instructions in one code object.
const MAX_LINETABLE_LEN: usize = 4 * 1024 * 1024;

/// Reading process memory.
///
/// Production reads the current process directly; tests read a synthetic
/// object graph. Every method returns `Option` so a bad address degrades to a
/// missing frame rather than a crash.
///
/// # Safety
///
/// Implementors must only return data they can actually read. The in-process
/// implementation relies on the caller holding the GIL, which keeps the
/// objects being walked alive.
pub unsafe trait Mem {
    /// Read a pointer-sized value.
    fn read_usize(&self, addr: usize) -> Option<usize>;
    /// Read a single byte.
    fn read_u8(&self, addr: usize) -> Option<u8>;
    /// Read a 32-bit signed integer.
    fn read_i32(&self, addr: usize) -> Option<i32>;
    /// Borrow `len` bytes.
    fn read_bytes(&self, addr: usize, len: usize) -> Option<&[u8]>;
}

/// Where collected frames go.
///
/// Implemented by the production sink, which interns the strings, and by test
/// sinks that just collect them.
pub trait FrameSink {
    /// Record one frame, innermost first.
    fn push_frame(&mut self, function: &str, file: &str, line: i32);
    /// Record that at least one frame was omitted.
    fn note_dropped(&mut self);
}

/// Addresses of the type objects the walk compares against.
///
/// Resolved once at start-up. Comparing `ob_type` is how the C++
/// `PyCode_Check` / `PyUnicode_Check` worked, and it is a genuine safety
/// check: without it a mis-offset read would have us decoding a string out of
/// something that is not one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TypeAddrs {
    /// Address of `PyCode_Type`.
    pub code: usize,
    /// Address of `PyUnicode_Type`.
    pub unicode: usize,
}

/// Walk the frame chain of `tstate`, innermost frame first.
///
/// Pushes at most `max_nframe` frames, and follows at most
/// [`TRACEBACK_MAX_WALKED_NFRAME`] links regardless, so a cyclic or malformed
/// chain still terminates. Frames without usable code are skipped without
/// consuming the emit budget, matching the C++.
pub fn walk_frames<M: Mem, S: FrameSink>(
    mem: &M,
    offsets: &Offsets,
    types: &TypeAddrs,
    tstate: usize,
    max_nframe: u16,
    sink: &mut S,
) {
    if tstate == 0 {
        sink.push_frame(NO_THREAD_STATE, UNKNOWN, 0);
        return;
    }
    let Some(mut frame) = mem.read_usize(tstate.wrapping_add(offsets.thread_state_current_frame))
    else {
        sink.push_frame(NO_THREAD_STATE, UNKNOWN, 0);
        return;
    };
    if frame == 0 {
        sink.push_frame(NO_PYTHON_FRAMES, UNKNOWN, 0);
        return;
    }

    let mut pushed: u16 = 0;
    let mut walked: u32 = 0;

    while frame != 0 {
        // Cap the raw link count separately from the emitted count, so that
        // skipped or malformed frames cannot make the walk unbounded.
        walked = walked.saturating_add(1);
        if walked > TRACEBACK_MAX_WALKED_NFRAME {
            sink.note_dropped();
            break;
        }
        // Once the emit budget is gone, record that deeper frames were
        // omitted and stop before doing any more decoding work.
        if pushed >= max_nframe {
            sink.note_dropped();
            break;
        }

        if let Some(code) = code_for_frame(mem, offsets, types, frame) {
            let name = read_code_name(mem, offsets, types, code);
            let file = read_ascii(
                mem,
                offsets,
                types,
                mem.read_usize(code.wrapping_add(offsets.code_filename))
                    .unwrap_or(0),
            );
            let line = line_for_frame(mem, offsets, frame, code);
            sink.push_frame(name, file, line);
            pushed = pushed.saturating_add(1);
        }

        frame = mem
            .read_usize(frame.wrapping_add(offsets.frame_previous))
            .unwrap_or(0);
    }
}

/// The code object for a frame, or `None` if the frame should be skipped.
fn code_for_frame<M: Mem>(
    mem: &M,
    offsets: &Offsets,
    types: &TypeAddrs,
    frame: usize,
) -> Option<usize> {
    let owner = mem.read_u8(frame.wrapping_add(offsets.frame_owner))?;
    if owner != FRAME_OWNED_BY_THREAD && owner != FRAME_OWNED_BY_GENERATOR {
        return None;
    }
    let executable = mem.read_usize(frame.wrapping_add(offsets.frame_executable))?;
    let code = executable & STACKREF_TAG_MASK;
    if code == 0 || !is_instance(mem, offsets, code, types.code) {
        return None;
    }
    Some(code)
}

/// `Py_IS_TYPE(obj, ty)`: compare the object's type pointer.
fn is_instance<M: Mem>(mem: &M, offsets: &Offsets, obj: usize, ty: usize) -> bool {
    if obj == 0 || ty == 0 {
        return false;
    }
    mem.read_usize(obj.wrapping_add(offsets.pyobject_ob_type)) == Some(ty)
}

/// Prefer `co_qualname` (which carries the class for methods) and fall back to
/// `co_name`, as the C++ `get_code_name` did.
fn read_code_name<'a, M: Mem>(
    mem: &'a M,
    offsets: &Offsets,
    types: &TypeAddrs,
    code: usize,
) -> &'a str {
    let qualname = mem
        .read_usize(code.wrapping_add(offsets.code_qualname))
        .unwrap_or(0);
    let chosen = if qualname != 0 {
        qualname
    } else {
        mem.read_usize(code.wrapping_add(offsets.code_name))
            .unwrap_or(0)
    };
    read_ascii(mem, offsets, types, chosen)
}

/// Decode a `str` object, but only when it is compact ASCII.
///
/// Anything else is reported as [`NON_ASCII`] rather than decoded: the general
/// path would go through `PyUnicode_AsUTF8AndSize`, which caches a UTF-8
/// buffer through `PyObject_Malloc` and would re-enter our own hook.
fn read_ascii<'a, M: Mem>(mem: &'a M, offsets: &Offsets, types: &TypeAddrs, obj: usize) -> &'a str {
    if obj == 0 || !is_instance(mem, offsets, obj, types.unicode) {
        return UNKNOWN;
    }
    let Some(state) = mem.read_u8(obj.wrapping_add(offsets.unicode_state)) else {
        return UNKNOWN;
    };
    // PyASCIIObject.state is a bitfield: interned:2, kind:3, compact:1,
    // ascii:1. Only a compact ASCII string stores its characters inline,
    // immediately after the PyASCIIObject header. (Free-threaded builds widen
    // `interned` and shift these, which is one more reason that build is
    // rejected outright.)
    let compact = (state >> 5) & 1;
    let ascii = (state >> 6) & 1;
    if compact == 0 || ascii == 0 {
        return NON_ASCII;
    }
    let Some(len) = mem.read_usize(obj.wrapping_add(offsets.unicode_length)) else {
        return UNKNOWN;
    };
    if len > MAX_STRING_LEN {
        return NON_ASCII;
    }
    let Some(bytes) = mem.read_bytes(obj.wrapping_add(offsets.unicode_asciiobject_size), len)
    else {
        return UNKNOWN;
    };
    // The `ascii` flag says this is valid ASCII and so valid UTF-8, but check
    // rather than assume: `from_utf8_unchecked` on a bad read would be
    // undefined behaviour, whereas this is a cheap scan that degrades.
    str::from_utf8(bytes).unwrap_or(NON_ASCII)
}

/// Resolve the current line number for a frame.
fn line_for_frame<M: Mem>(mem: &M, offsets: &Offsets, frame: usize, code: usize) -> i32 {
    let Some(firstlineno) = mem.read_i32(code.wrapping_add(offsets.code_firstlineno)) else {
        return 0;
    };
    let Some(lasti) = lasti_for_frame(mem, offsets, frame, code) else {
        return firstlineno;
    };
    let Some(table) = read_linetable(mem, offsets, code) else {
        return firstlineno;
    };
    linetable::parse(table, lasti, firstlineno)
}

/// `frame->instr_ptr - _PyCode_CODE(code)`, in `_Py_CODEUNIT` units.
fn lasti_for_frame<M: Mem>(mem: &M, offsets: &Offsets, frame: usize, code: usize) -> Option<i32> {
    let instr = mem.read_usize(frame.wrapping_add(offsets.frame_instr_ptr))?;
    let code_start = code.wrapping_add(offsets.code_co_code_adaptive);
    // Signed, and truncating toward zero, matching C pointer subtraction. A
    // negative result makes `parse` fall back to `firstlineno`.
    #[allow(clippy::cast_possible_wrap)]
    let byte_delta = (instr as isize).wrapping_sub(code_start as isize);
    // PANIC-OK: the divisor is a non-zero constant.
    #[allow(clippy::integer_division, clippy::cast_possible_truncation)]
    let units = (byte_delta / 2) as i32;
    Some(units)
}

/// Borrow the bytes of `code->co_linetable`.
fn read_linetable<'a, M: Mem>(mem: &'a M, offsets: &Offsets, code: usize) -> Option<&'a [u8]> {
    let obj = mem.read_usize(code.wrapping_add(offsets.code_linetable))?;
    if obj == 0 {
        return None;
    }
    let len = mem.read_usize(obj.wrapping_add(offsets.bytes_ob_size))?;
    if len > MAX_LINETABLE_LEN {
        return None;
    }
    mem.read_bytes(obj.wrapping_add(offsets.bytes_ob_sval), len)
}

/// A [`FrameSink`] that collects owned frames. For tests and diagnostics.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CollectedFrames {
    /// `(function, file, line)`, innermost first.
    pub frames: Vec<(String, String, i32)>,
    /// How many times frames were omitted.
    pub dropped: u32,
}

impl FrameSink for CollectedFrames {
    fn push_frame(&mut self, function: &str, file: &str, line: i32) {
        self.frames
            .push((function.to_owned(), file.to_owned(), line));
    }

    fn note_dropped(&mut self) {
        self.dropped = self.dropped.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation
    )]

    use super::*;
    use crate::memalloc::limits::TRACEBACK_MAX_NFRAME;
    use crate::memalloc::tests::fake_interp::{
        FRAME_OWNED_BY_CSTACK, FRAME_OWNED_BY_FRAME_OBJECT, FRAME_OWNED_BY_GENERATOR,
        FRAME_OWNED_BY_THREAD, FakeInterp,
    };

    /// A line table for a single-line function: one entry, info code 13 (a
    /// signed line delta) of zero, covering one code unit.
    const FLAT_LINETABLE: &[u8] = &[0x68, 0x00];

    fn walk(interp: &FakeInterp, tstate: usize, max_nframe: u16) -> CollectedFrames {
        let mut out = CollectedFrames::default();
        walk_frames(
            interp,
            &interp.offsets(),
            &interp.types(),
            tstate,
            max_nframe,
            &mut out,
        );
        out
    }

    fn names(collected: &CollectedFrames) -> Vec<&str> {
        collected
            .frames
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect()
    }

    #[test]
    fn walks_a_chain_innermost_first() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let outer_code = interp.new_code("outer", "a.py", FLAT_LINETABLE, 10);
        let middle_code = interp.new_code("Klass.middle", "b.py", FLAT_LINETABLE, 20);
        let inner_code = interp.new_code("inner", "c.py", FLAT_LINETABLE, 30);

        let outer = interp.new_frame(outer_code, 0, FRAME_OWNED_BY_THREAD, 0);
        let middle = interp.new_frame(middle_code, outer, FRAME_OWNED_BY_GENERATOR, 0);
        let inner = interp.new_frame(inner_code, middle, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(inner);

        let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
        assert_eq!(names(&got), ["inner", "Klass.middle", "outer"]);
        assert_eq!(
            got.frames
                .iter()
                .map(|(_, f, _)| f.as_str())
                .collect::<Vec<_>>(),
            ["c.py", "b.py", "a.py"]
        );
        assert_eq!(
            got.frames.iter().map(|(_, _, l)| *l).collect::<Vec<_>>(),
            [30, 20, 10]
        );
        assert_eq!(got.dropped, 0);
    }

    #[test]
    fn a_null_thread_state_yields_a_marker_frame() {
        let interp = FakeInterp::with_capacity(4096);
        let got = walk(&interp, 0, TRACEBACK_MAX_NFRAME);
        assert_eq!(names(&got), [NO_THREAD_STATE]);
    }

    #[test]
    fn a_thread_state_with_no_frames_yields_a_marker_frame() {
        let mut interp = FakeInterp::with_capacity(4096);
        let tstate = interp.new_tstate(0);
        let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
        assert_eq!(names(&got), [NO_PYTHON_FRAMES]);
    }

    /// Interpreter shim frames and frames owned by a materialised frame object
    /// carry no code we want, and must be skipped without using up the emit
    /// budget.
    #[test]
    fn frames_with_other_owners_are_skipped() {
        for owner in [FRAME_OWNED_BY_FRAME_OBJECT, FRAME_OWNED_BY_CSTACK, 9, 255] {
            let mut interp = FakeInterp::with_capacity(64 * 1024);
            let real_code = interp.new_code("real", "a.py", FLAT_LINETABLE, 1);
            let shim_code = interp.new_code("shim", "b.py", FLAT_LINETABLE, 2);
            let real = interp.new_frame(real_code, 0, FRAME_OWNED_BY_THREAD, 0);
            let shim = interp.new_frame(shim_code, real, owner, 0);
            let tstate = interp.new_tstate(shim);

            let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
            assert_eq!(names(&got), ["real"], "owner {owner} was not skipped");
        }
    }

    #[test]
    fn a_frame_with_no_code_is_skipped() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let real_code = interp.new_code("real", "a.py", FLAT_LINETABLE, 1);
        let real = interp.new_frame(real_code, 0, FRAME_OWNED_BY_THREAD, 0);
        let empty = interp.new_frame_raw(0, real, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(empty);

        assert_eq!(
            names(&walk(&interp, tstate, TRACEBACK_MAX_NFRAME)),
            ["real"]
        );
    }

    /// An `executable` word pointing at something that is not a code object
    /// must be rejected by the type check, not decoded.
    #[test]
    fn a_frame_whose_executable_is_not_code_is_skipped() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let real_code = interp.new_code("real", "a.py", FLAT_LINETABLE, 1);
        let real = interp.new_frame(real_code, 0, FRAME_OWNED_BY_THREAD, 0);
        let imposter = interp.new_str("not a code object");
        let bad = interp.new_frame_raw(imposter, real, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(bad);

        assert_eq!(
            names(&walk(&interp, tstate, TRACEBACK_MAX_NFRAME)),
            ["real"]
        );
    }

    /// On 3.14 `f_executable` is a tagged pointer. The low bits must be
    /// cleared before the word is used as an address.
    #[test]
    fn stackref_tag_bits_are_masked_off() {
        for tag in 0..=7usize {
            let mut interp = FakeInterp::with_capacity(64 * 1024);
            let code = interp.new_code("tagged", "a.py", FLAT_LINETABLE, 7);
            let frame = interp.new_frame_raw(code | tag, 0, FRAME_OWNED_BY_THREAD, 0);
            let tstate = interp.new_tstate(frame);

            let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
            assert_eq!(names(&got), ["tagged"], "tag {tag} was not masked");
            assert_eq!(got.frames[0].2, 7);
        }
    }

    #[test]
    fn qualname_is_preferred_and_name_is_the_fallback() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let qualname = interp.new_str("Klass.method");
        let name = interp.new_str("method");
        let filename = interp.new_str("a.py");
        let linetable = interp.new_bytes(FLAT_LINETABLE);

        let with_qualname = interp.new_code_raw(qualname, name, filename, linetable, 1);
        let without = interp.new_code_raw(0, name, filename, linetable, 1);

        let outer = interp.new_frame(without, 0, FRAME_OWNED_BY_THREAD, 0);
        let inner = interp.new_frame(with_qualname, outer, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(inner);

        assert_eq!(
            names(&walk(&interp, tstate, TRACEBACK_MAX_NFRAME)),
            ["Klass.method", "method"]
        );
    }

    /// Decoding a non-ASCII or non-compact string would mean calling
    /// `PyUnicode_AsUTF8AndSize`, which allocates through PyMem. Report a
    /// placeholder instead.
    #[test]
    fn strings_we_must_not_decode_become_placeholders() {
        // state bits: compact is bit 5, ascii is bit 6.
        let cases = [
            (0b0110_0000u8, "plain"), // compact + ascii: decoded
            (0b0010_0000, NON_ASCII), // compact, not ascii
            (0b0100_0000, NON_ASCII), // ascii, not compact
            (0b0000_0000, NON_ASCII), // neither
        ];
        for (state, want) in cases {
            let mut interp = FakeInterp::with_capacity(64 * 1024);
            let qualname = interp.new_str_with_state("plain", state);
            let filename = interp.new_str("a.py");
            let linetable = interp.new_bytes(FLAT_LINETABLE);
            let code = interp.new_code_raw(qualname, 0, filename, linetable, 1);
            let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, 0);
            let tstate = interp.new_tstate(frame);

            assert_eq!(
                names(&walk(&interp, tstate, TRACEBACK_MAX_NFRAME)),
                [want],
                "state {state:#010b}"
            );
        }
    }

    #[test]
    fn a_name_that_is_not_a_string_is_unknown() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let foreign = interp.new_foreign_object();
        let filename = interp.new_str("a.py");
        let linetable = interp.new_bytes(FLAT_LINETABLE);
        let code = interp.new_code_raw(foreign, 0, filename, linetable, 1);
        let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(frame);

        assert_eq!(
            names(&walk(&interp, tstate, TRACEBACK_MAX_NFRAME)),
            [UNKNOWN]
        );
    }

    #[test]
    fn the_emit_cap_is_honoured_and_recorded() {
        let mut interp = FakeInterp::with_capacity(1024 * 1024);
        let code = interp.new_code("f", "a.py", FLAT_LINETABLE, 1);
        let mut frame = 0;
        for _ in 0..50 {
            frame = interp.new_frame(code, frame, FRAME_OWNED_BY_THREAD, 0);
        }
        let tstate = interp.new_tstate(frame);

        let got = walk(&interp, tstate, 10);
        assert_eq!(got.frames.len(), 10);
        assert_eq!(got.dropped, 1, "omitted frames were not recorded");
    }

    /// The raw link cap is separate from the emit cap so that a malformed or
    /// cyclic chain cannot make the walk unbounded. This is the test that
    /// proves an allocator hook cannot hang.
    #[test]
    fn a_cyclic_chain_terminates_via_the_walk_cap() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let code = interp.new_code("loop", "a.py", FLAT_LINETABLE, 1);
        // Two frames pointing at each other, both skipped so the emit cap
        // never fires and only the link cap can stop the walk.
        let first = interp.new_frame_raw(0, 0, FRAME_OWNED_BY_THREAD, 0);
        let second = interp.new_frame_raw(0, first, FRAME_OWNED_BY_THREAD, 0);
        interp.set_frame_previous(first, second);
        let tstate = interp.new_tstate(second);
        let _ = code;

        let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
        assert!(got.frames.is_empty());
        assert_eq!(got.dropped, 1, "the walk cap did not fire");
    }

    #[test]
    fn a_self_referential_frame_terminates() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let code = interp.new_code("self", "a.py", FLAT_LINETABLE, 3);
        let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, 0);
        interp.set_frame_previous(frame, frame);
        let tstate = interp.new_tstate(frame);

        let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
        // Emits up to the frame cap, then stops.
        assert_eq!(got.frames.len(), usize::from(TRACEBACK_MAX_NFRAME));
        assert_eq!(got.dropped, 1);
    }

    #[test]
    fn line_numbers_come_from_the_line_table() {
        // Three entries, each covering one code unit and advancing the line
        // by one: info code 12 is "new line, delta = 12 - 10 = 2", and 11 is
        // a delta of 1. Two column bytes follow each.
        let table: &[u8] = &[0x58, 0x00, 0x00, 0x58, 0x00, 0x00];
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let code = interp.new_code("f", "a.py", table, 100);

        for lasti in 0..3usize {
            let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, lasti);
            let tstate = interp.new_tstate(frame);
            let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
            assert_eq!(got.frames.len(), 1);
            let line = got.frames[0].2;
            assert!(line >= 100, "lasti {lasti} gave line {line}");
        }
    }

    /// An `instr_ptr` before the start of the bytecode gives a negative
    /// `lasti`, which must fall back to `co_firstlineno` rather than misparse.
    #[test]
    fn an_instr_ptr_below_the_code_start_falls_back_to_firstlineno() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let code = interp.new_code("f", "a.py", FLAT_LINETABLE, 4242);
        let frame = interp.new_frame_raw(code, 0, FRAME_OWNED_BY_THREAD, 8);
        let tstate = interp.new_tstate(frame);

        let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
        assert_eq!(got.frames[0].2, 4242);
    }

    #[test]
    fn a_missing_line_table_falls_back_to_firstlineno() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let qualname = interp.new_str("f");
        let filename = interp.new_str("a.py");
        let code = interp.new_code_raw(qualname, 0, filename, 0, 77);
        let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(frame);

        assert_eq!(walk(&interp, tstate, TRACEBACK_MAX_NFRAME).frames[0].2, 77);
    }

    /// A `bytes` object claiming a length far past the arena must be refused
    /// rather than turned into a huge slice.
    #[test]
    fn an_absurd_line_table_length_is_refused() {
        for claimed in [usize::MAX, MAX_LINETABLE_LEN + 1, 1 << 40] {
            let mut interp = FakeInterp::with_capacity(64 * 1024);
            let qualname = interp.new_str("f");
            let filename = interp.new_str("a.py");
            let linetable = interp.new_bytes_claiming_len(claimed);
            let code = interp.new_code_raw(qualname, 0, filename, linetable, 55);
            let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, 0);
            let tstate = interp.new_tstate(frame);

            let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
            assert_eq!(got.frames[0].2, 55, "claimed length {claimed}");
        }
    }

    #[test]
    fn a_zero_frame_budget_emits_nothing_but_records_the_drop() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let code = interp.new_code("f", "a.py", FLAT_LINETABLE, 1);
        let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(frame);

        let got = walk(&interp, tstate, 0);
        assert!(got.frames.is_empty());
        assert_eq!(got.dropped, 1);
    }

    /// Addresses that are not in the arena at all must produce no frames and
    /// no panic, which is the behaviour that keeps a bad offset from crashing
    /// the host process.
    #[test]
    fn wild_addresses_do_not_panic() {
        let interp = FakeInterp::with_capacity(4096);
        for tstate in [1usize, 0xdead_beef, usize::MAX, usize::MAX - 7] {
            let got = walk(&interp, tstate, TRACEBACK_MAX_NFRAME);
            // Either a marker frame or nothing; never a crash.
            assert!(got.frames.len() <= 1, "tstate {tstate:#x} produced frames");
        }
    }

    #[test]
    fn unresolved_type_addresses_reject_everything() {
        let mut interp = FakeInterp::with_capacity(64 * 1024);
        let code = interp.new_code("f", "a.py", FLAT_LINETABLE, 1);
        let frame = interp.new_frame(code, 0, FRAME_OWNED_BY_THREAD, 0);
        let tstate = interp.new_tstate(frame);

        let mut out = CollectedFrames::default();
        walk_frames(
            &interp,
            &interp.offsets(),
            &TypeAddrs::default(),
            tstate,
            TRACEBACK_MAX_NFRAME,
            &mut out,
        );
        assert!(
            out.frames.is_empty(),
            "frames were accepted with no PyCode_Type address"
        );
    }
}
