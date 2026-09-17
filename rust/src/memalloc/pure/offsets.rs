//! Reading CPython's `_Py_DebugOffsets`.
//!
//! Rust cannot include CPython's internal headers, so it cannot hardcode where
//! `_PyInterpreterFrame.previous` or `PyCodeObject.co_linetable` live. CPython
//! 3.13+ solves this itself: the first member of `_PyRuntimeState` is a
//! `_Py_DebugOffsets`, a self-describing table of `offsetof()` values that
//! exists so out-of-process debuggers and profilers do not have to know the
//! layout. From `Include/internal/pycore_runtime.h`:
//!
//! > This field must be first to facilitate locating it by out of process
//! > debuggers. [...] This struct is only guaranteed to be stable between
//! > patch versions for a given minor version of the interpreter.
//!
//! That stability contract is why the mirrors below are per minor version, and
//! why the selection fails closed: a mirror is only selected for a version we
//! have actually transcribed, so a future CPython lands in the unsupported arm
//! rather than having a 3.14 layout read out of it.
//!
//! The structs mirror the header's nesting so they can be diffed against it by
//! eye. Groups whose layout is identical across versions are shared; the ones
//! CPython extended in 3.14 are duplicated.

use core::ffi::c_char;

/// Magic value at the start of a valid `_Py_DebugOffsets`.
pub const COOKIE: &[u8; 8] = b"xdebugpy";

/// Plausible upper bound for any `sizeof()` reported in the table.
///
/// `PyThreadState` is the largest struct we check and runs to a few hundred
/// bytes; past this we are certainly not looking at a real offsets table.
const MAX_PLAUSIBLE_STRUCT_SIZE: u64 = 8192;

// --- groups with the same layout on every supported version -----------------

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct RuntimeState {
    pub size: u64,
    pub finalizing: u64,
    pub interpreters_head: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct ThreadState {
    pub size: u64,
    pub prev: u64,
    pub next: u64,
    pub interp: u64,
    pub current_frame: u64,
    pub thread_id: u64,
    pub native_thread_id: u64,
    pub datastack_chunk: u64,
    pub status: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct PyObjectOffsets {
    pub size: u64,
    pub ob_type: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct TypeObject {
    pub size: u64,
    pub tp_name: u64,
    pub tp_repr: u64,
    pub tp_flags: u64,
}

/// `tuple_object` and `list_object`, which share a shape.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct SequenceObject {
    pub size: u64,
    pub ob_item: u64,
    pub ob_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct DictObject {
    pub size: u64,
    pub ma_keys: u64,
    pub ma_values: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct FloatObject {
    pub size: u64,
    pub ob_fval: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct LongObject {
    pub size: u64,
    pub lv_tag: u64,
    pub ob_digit: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct BytesObject {
    pub size: u64,
    pub ob_size: u64,
    pub ob_sval: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct UnicodeObject {
    pub size: u64,
    pub state: u64,
    pub length: u64,
    /// `sizeof(PyASCIIObject)`, i.e. where a compact ASCII string's characters
    /// begin. A size, not an offset.
    pub asciiobject_size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct GcOffsets {
    pub size: u64,
    pub collecting: u64,
}

// --- groups CPython extended in 3.14 ----------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct InterpreterStateV313 {
    pub size: u64,
    pub id: u64,
    pub next: u64,
    pub threads_head: u64,
    pub gc: u64,
    pub imports_modules: u64,
    pub sysdict: u64,
    pub builtins: u64,
    pub ceval_gil: u64,
    pub gil_runtime_state: u64,
    pub gil_runtime_state_enabled: u64,
    pub gil_runtime_state_locked: u64,
    pub gil_runtime_state_holder: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct InterpreterStateV314 {
    pub size: u64,
    pub id: u64,
    pub next: u64,
    pub threads_head: u64,
    pub threads_main: u64,
    pub gc: u64,
    pub imports_modules: u64,
    pub sysdict: u64,
    pub builtins: u64,
    pub ceval_gil: u64,
    pub gil_runtime_state: u64,
    pub gil_runtime_state_enabled: u64,
    pub gil_runtime_state_locked: u64,
    pub gil_runtime_state_holder: u64,
    pub code_object_generation: u64,
    pub tlbc_generation: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct InterpreterFrameV313 {
    pub size: u64,
    pub previous: u64,
    pub executable: u64,
    pub instr_ptr: u64,
    pub localsplus: u64,
    pub owner: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct InterpreterFrameV314 {
    pub size: u64,
    pub previous: u64,
    pub executable: u64,
    pub instr_ptr: u64,
    pub localsplus: u64,
    pub owner: u64,
    pub stackpointer: u64,
    pub tlbc_index: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct CodeObjectV313 {
    pub size: u64,
    pub filename: u64,
    pub name: u64,
    pub qualname: u64,
    pub linetable: u64,
    pub firstlineno: u64,
    pub argcount: u64,
    pub localsplusnames: u64,
    pub localspluskinds: u64,
    pub co_code_adaptive: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct CodeObjectV314 {
    pub size: u64,
    pub filename: u64,
    pub name: u64,
    pub qualname: u64,
    pub linetable: u64,
    pub firstlineno: u64,
    pub argcount: u64,
    pub localsplusnames: u64,
    pub localspluskinds: u64,
    pub co_code_adaptive: u64,
    pub co_tlbc: u64,
}

// --- groups that exist only on 3.14 -----------------------------------------

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct SetObject {
    pub size: u64,
    pub used: u64,
    pub table: u64,
    pub mask: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct GenObject {
    pub size: u64,
    pub gi_name: u64,
    pub gi_iframe: u64,
    pub gi_frame_state: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct LlistNode {
    pub next: u64,
    pub prev: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct DebuggerSupport {
    pub eval_breaker: u64,
    pub remote_debugger_support: u64,
    pub remote_debugging_enabled: u64,
    pub debugger_pending_call: u64,
    pub debugger_script_path: u64,
    pub debugger_script_path_size: u64,
}

// --- the mirrors themselves -------------------------------------------------
//
// Transcribed from:
//   3.13: Include/internal/pycore_runtime.h
//   3.14: Include/internal/pycore_debug_offsets.h
//
// `scripts/check_debug_offsets.py` cross-checks the extracted values against a
// live interpreter through ctypes, so a transcription error surfaces as a loud
// failure rather than a corrupt profile.

/// Mirror of `_Py_DebugOffsets` on CPython 3.13.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct DebugOffsetsV313 {
    pub cookie: [c_char; 8],
    pub version: u64,
    pub free_threaded: u64,
    pub runtime_state: RuntimeState,
    pub interpreter_state: InterpreterStateV313,
    pub thread_state: ThreadState,
    pub interpreter_frame: InterpreterFrameV313,
    pub code_object: CodeObjectV313,
    pub pyobject: PyObjectOffsets,
    pub type_object: TypeObject,
    pub tuple_object: SequenceObject,
    pub list_object: SequenceObject,
    pub dict_object: DictObject,
    pub float_object: FloatObject,
    pub long_object: LongObject,
    pub bytes_object: BytesObject,
    pub unicode_object: UnicodeObject,
    pub gc: GcOffsets,
}

/// Mirror of `_Py_DebugOffsets` on CPython 3.14.
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct DebugOffsetsV314 {
    pub cookie: [c_char; 8],
    pub version: u64,
    pub free_threaded: u64,
    pub runtime_state: RuntimeState,
    pub interpreter_state: InterpreterStateV314,
    pub thread_state: ThreadState,
    pub interpreter_frame: InterpreterFrameV314,
    pub code_object: CodeObjectV314,
    pub pyobject: PyObjectOffsets,
    pub type_object: TypeObject,
    pub tuple_object: SequenceObject,
    pub list_object: SequenceObject,
    pub set_object: SetObject,
    pub dict_object: DictObject,
    pub float_object: FloatObject,
    pub long_object: LongObject,
    pub bytes_object: BytesObject,
    pub unicode_object: UnicodeObject,
    pub gc: GcOffsets,
    pub gen_object: GenObject,
    pub llist_node: LlistNode,
    pub debugger_support: DebuggerSupport,
}

// --- flattening and validation ----------------------------------------------

/// The version-specific fields the profiler needs, pulled out of a mirror.
///
/// Exists so the validation below is written once rather than per version.
#[derive(Clone, Copy)]
struct Raw {
    cookie: [u8; 8],
    version: u64,
    free_threaded: u64,
    thread_state: ThreadState,
    frame_size: u64,
    frame_previous: u64,
    frame_executable: u64,
    frame_instr_ptr: u64,
    frame_owner: u64,
    code_size: u64,
    code_filename: u64,
    code_name: u64,
    code_qualname: u64,
    code_linetable: u64,
    code_firstlineno: u64,
    code_co_code_adaptive: u64,
    pyobject: PyObjectOffsets,
    type_object: TypeObject,
    bytes_object: BytesObject,
    unicode_object: UnicodeObject,
}

impl DebugOffsetsV313 {
    fn raw(&self) -> Raw {
        Raw {
            cookie: self.cookie.map(|c| c as u8),
            version: self.version,
            free_threaded: self.free_threaded,
            thread_state: self.thread_state,
            frame_size: self.interpreter_frame.size,
            frame_previous: self.interpreter_frame.previous,
            frame_executable: self.interpreter_frame.executable,
            frame_instr_ptr: self.interpreter_frame.instr_ptr,
            frame_owner: self.interpreter_frame.owner,
            code_size: self.code_object.size,
            code_filename: self.code_object.filename,
            code_name: self.code_object.name,
            code_qualname: self.code_object.qualname,
            code_linetable: self.code_object.linetable,
            code_firstlineno: self.code_object.firstlineno,
            code_co_code_adaptive: self.code_object.co_code_adaptive,
            pyobject: self.pyobject,
            type_object: self.type_object,
            bytes_object: self.bytes_object,
            unicode_object: self.unicode_object,
        }
    }

    /// Validate this table and flatten out the fields the profiler needs.
    pub fn extract(&self, want: (u8, u8)) -> Result<Offsets, OffsetsError> {
        validate(self.raw(), want)
    }
}

impl DebugOffsetsV314 {
    fn raw(&self) -> Raw {
        Raw {
            cookie: self.cookie.map(|c| c as u8),
            version: self.version,
            free_threaded: self.free_threaded,
            thread_state: self.thread_state,
            frame_size: self.interpreter_frame.size,
            frame_previous: self.interpreter_frame.previous,
            frame_executable: self.interpreter_frame.executable,
            frame_instr_ptr: self.interpreter_frame.instr_ptr,
            frame_owner: self.interpreter_frame.owner,
            code_size: self.code_object.size,
            code_filename: self.code_object.filename,
            code_name: self.code_object.name,
            code_qualname: self.code_object.qualname,
            code_linetable: self.code_object.linetable,
            code_firstlineno: self.code_object.firstlineno,
            code_co_code_adaptive: self.code_object.co_code_adaptive,
            pyobject: self.pyobject,
            type_object: self.type_object,
            bytes_object: self.bytes_object,
            unicode_object: self.unicode_object,
        }
    }

    /// Validate this table and flatten out the fields the profiler needs.
    pub fn extract(&self, want: (u8, u8)) -> Result<Offsets, OffsetsError> {
        validate(self.raw(), want)
    }
}

/// The subset of `_Py_DebugOffsets` the profiler needs, version-independent.
///
/// Extracted once at start-up. The frame walker takes it by reference, which
/// also makes it trivial to construct in tests with no interpreter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Offsets {
    pub thread_state_size: usize,
    pub thread_state_current_frame: usize,
    pub frame_size: usize,
    pub frame_previous: usize,
    pub frame_executable: usize,
    pub frame_instr_ptr: usize,
    pub frame_owner: usize,
    pub code_size: usize,
    pub code_filename: usize,
    pub code_name: usize,
    pub code_qualname: usize,
    pub code_linetable: usize,
    pub code_firstlineno: usize,
    pub code_co_code_adaptive: usize,
    pub bytes_ob_size: usize,
    pub bytes_ob_sval: usize,
    pub unicode_size: usize,
    pub unicode_state: usize,
    pub unicode_length: usize,
    pub unicode_asciiobject_size: usize,
    pub pyobject_ob_type: usize,
    pub type_tp_flags: usize,
}

/// Why the offsets table could not be used.
///
/// Every variant is a soft failure: the caller declines to install the
/// allocator hooks and memory profiling degrades to a logged warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffsetsError {
    /// This build has no transcribed mirror for the target CPython.
    UnsupportedVersion { major: u8, minor: u8 },
    /// `_PyRuntime` was not resolvable.
    SymbolMissing,
    /// The leading cookie was not `xdebugpy`, so this is not an offsets table.
    /// Almost always means the interpreter predates 3.13, where the table was
    /// introduced.
    BadCookie([u8; 8]),
    /// The interpreter is not the minor version this build was compiled
    /// against, so the mirror does not describe it.
    VersionMismatch { got: (u8, u8), want: (u8, u8) },
    /// A free-threaded build. The allocator hook relies on the GIL.
    FreeThreaded,
    /// A reported `sizeof()` is not believable.
    ImplausibleSize { group: &'static str, size: u64 },
    /// A field offset lies outside its own struct.
    OffsetOutOfRange {
        group: &'static str,
        field: &'static str,
        offset: u64,
        size: u64,
    },
}

fn validate(raw: Raw, want: (u8, u8)) -> Result<Offsets, OffsetsError> {
    if &raw.cookie != COOKIE {
        return Err(OffsetsError::BadCookie(raw.cookie));
    }

    // PY_VERSION_HEX is 0xMMmmpprs.
    #[allow(clippy::cast_possible_truncation)]
    let got = (
        ((raw.version >> 24) & 0xff) as u8,
        ((raw.version >> 16) & 0xff) as u8,
    );
    if got != want {
        return Err(OffsetsError::VersionMismatch { got, want });
    }

    if raw.free_threaded != 0 {
        return Err(OffsetsError::FreeThreaded);
    }

    // A real table reports believable sizes. This is the check that catches a
    // drifted mirror: its fields would land on the wrong slots, and the values
    // that came back as sizes would be pointers or garbage.
    for (group, size) in [
        ("thread_state", raw.thread_state.size),
        ("interpreter_frame", raw.frame_size),
        ("code_object", raw.code_size),
        ("unicode_object", raw.unicode_object.size),
        ("bytes_object", raw.bytes_object.size),
        ("type_object", raw.type_object.size),
        ("pyobject", raw.pyobject.size),
        (
            "unicode_object.asciiobject_size",
            raw.unicode_object.asciiobject_size,
        ),
    ] {
        if size == 0 || size > MAX_PLAUSIBLE_STRUCT_SIZE {
            return Err(OffsetsError::ImplausibleSize { group, size });
        }
    }

    // And every field we dereference must lie inside its own struct.
    for (group, field, offset, size) in [
        (
            "thread_state",
            "current_frame",
            raw.thread_state.current_frame,
            raw.thread_state.size,
        ),
        (
            "interpreter_frame",
            "previous",
            raw.frame_previous,
            raw.frame_size,
        ),
        (
            "interpreter_frame",
            "executable",
            raw.frame_executable,
            raw.frame_size,
        ),
        (
            "interpreter_frame",
            "instr_ptr",
            raw.frame_instr_ptr,
            raw.frame_size,
        ),
        (
            "interpreter_frame",
            "owner",
            raw.frame_owner,
            raw.frame_size,
        ),
        ("code_object", "filename", raw.code_filename, raw.code_size),
        ("code_object", "name", raw.code_name, raw.code_size),
        ("code_object", "qualname", raw.code_qualname, raw.code_size),
        (
            "code_object",
            "linetable",
            raw.code_linetable,
            raw.code_size,
        ),
        (
            "code_object",
            "firstlineno",
            raw.code_firstlineno,
            raw.code_size,
        ),
        (
            "code_object",
            "co_code_adaptive",
            raw.code_co_code_adaptive,
            raw.code_size,
        ),
        (
            "bytes_object",
            "ob_size",
            raw.bytes_object.ob_size,
            raw.bytes_object.size,
        ),
        (
            "bytes_object",
            "ob_sval",
            raw.bytes_object.ob_sval,
            raw.bytes_object.size,
        ),
        (
            "unicode_object",
            "state",
            raw.unicode_object.state,
            raw.unicode_object.size,
        ),
        (
            "unicode_object",
            "length",
            raw.unicode_object.length,
            raw.unicode_object.size,
        ),
        (
            "pyobject",
            "ob_type",
            raw.pyobject.ob_type,
            raw.pyobject.size,
        ),
        (
            "type_object",
            "tp_flags",
            raw.type_object.tp_flags,
            raw.type_object.size,
        ),
    ] {
        if offset >= size {
            return Err(OffsetsError::OffsetOutOfRange {
                group,
                field,
                offset,
                size,
            });
        }
    }

    // PANIC-OK: every value is bounded by the checks above, and `usize` is
    // 64-bit on every platform we ship.
    #[allow(clippy::cast_possible_truncation)]
    Ok(Offsets {
        thread_state_size: raw.thread_state.size as usize,
        thread_state_current_frame: raw.thread_state.current_frame as usize,
        frame_size: raw.frame_size as usize,
        frame_previous: raw.frame_previous as usize,
        frame_executable: raw.frame_executable as usize,
        frame_instr_ptr: raw.frame_instr_ptr as usize,
        frame_owner: raw.frame_owner as usize,
        code_size: raw.code_size as usize,
        code_filename: raw.code_filename as usize,
        code_name: raw.code_name as usize,
        code_qualname: raw.code_qualname as usize,
        code_linetable: raw.code_linetable as usize,
        code_firstlineno: raw.code_firstlineno as usize,
        code_co_code_adaptive: raw.code_co_code_adaptive as usize,
        bytes_ob_size: raw.bytes_object.ob_size as usize,
        bytes_ob_sval: raw.bytes_object.ob_sval as usize,
        unicode_size: raw.unicode_object.size as usize,
        unicode_state: raw.unicode_object.state as usize,
        unicode_length: raw.unicode_object.length as usize,
        unicode_asciiobject_size: raw.unicode_object.asciiobject_size as usize,
        pyobject_ob_type: raw.pyobject.ob_type as usize,
        type_tp_flags: raw.type_object.tp_flags as usize,
    })
}

// --- version selection ------------------------------------------------------
//
// `build.rs` emits `pyroscope_py_minor_NN` only for versions transcribed above,
// so an untranscribed CPython gets the unsupported arm rather than a
// plausible-looking but wrong layout.

/// Version this build expects to find, or `None` if it has no mirror.
pub const WANT: Option<(u8, u8)> = if cfg!(pyroscope_py_minor_13) {
    Some((3, 13))
} else if cfg!(pyroscope_py_minor_14) {
    Some((3, 14))
} else {
    None
};

/// Which mirror this build uses.
#[cfg(pyroscope_py_minor_13)]
type TargetMirror = DebugOffsetsV313;
#[cfg(pyroscope_py_minor_14)]
type TargetMirror = DebugOffsetsV314;

/// Read and validate the offsets table at `base`.
///
/// # Safety
///
/// `base` must either be null or point at a readable `_Py_DebugOffsets` of at
/// least the mirror's size, i.e. the address of `_PyRuntime`.
#[cfg(any(pyroscope_py_minor_13, pyroscope_py_minor_14))]
pub unsafe fn read(base: *const u8) -> Result<Offsets, OffsetsError> {
    if base.is_null() {
        return Err(OffsetsError::SymbolMissing);
    }
    let Some(want) = WANT else {
        return Err(OffsetsError::UnsupportedVersion {
            major: build_major(),
            minor: build_minor(),
        });
    };
    // SAFETY: the caller guarantees `base` points at a table at least this
    // large. The mirror is `#[repr(C)]` with only integral fields, so every
    // bit pattern is a valid value and the read itself cannot be unsound even
    // when the contents are nonsense. `validate` is what rejects nonsense.
    let raw = unsafe { &*base.cast::<TargetMirror>() };
    raw.extract(want)
}

/// Stub for CPython versions with no transcribed mirror.
///
/// # Safety
///
/// Trivially safe: `base` is never dereferenced.
#[cfg(not(any(pyroscope_py_minor_13, pyroscope_py_minor_14)))]
pub unsafe fn read(_base: *const u8) -> Result<Offsets, OffsetsError> {
    Err(OffsetsError::UnsupportedVersion {
        major: build_major(),
        minor: build_minor(),
    })
}

/// Major CPython version this extension was built against.
pub fn build_major() -> u8 {
    env!("PYROSCOPE_PY_MAJOR").parse().unwrap_or(0)
}

/// Minor CPython version this extension was built against.
pub fn build_minor() -> u8 {
    env!("PYROSCOPE_PY_MINOR").parse().unwrap_or(0)
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
    use core::mem::{align_of, offset_of, size_of};

    /// Slot counts straight from the CPython headers. Every field of
    /// `_Py_DebugOffsets` is 8 bytes, so if a mirror gains or loses one, every
    /// offset after it shifts. This catches that offline, with no interpreter.
    #[test]
    fn mirror_sizes_match_the_headers() {
        // cookie + version + free_threaded, then the per-group field counts.
        let v313 = 3 + (3 + 13 + 9 + 6 + 10 + 2 + 4 + 3 + 3 + 3 + 2 + 3 + 3 + 4 + 2);
        let v314 =
            3 + (3 + 16 + 9 + 8 + 11 + 2 + 4 + 3 + 3 + 4 + 3 + 2 + 3 + 3 + 4 + 2 + 4 + 2 + 6);
        assert_eq!(v313, 73, "3.13 slot count");
        assert_eq!(v314, 95, "3.14 slot count");
        assert_eq!(size_of::<DebugOffsetsV313>(), v313 * 8);
        assert_eq!(size_of::<DebugOffsetsV314>(), v314 * 8);
        assert_eq!(align_of::<DebugOffsetsV313>(), 8);
        assert_eq!(align_of::<DebugOffsetsV314>(), 8);
    }

    /// Spot-check where the fields the walker dereferences actually land,
    /// against slot indices counted by hand from the headers.
    #[test]
    fn key_field_offsets_are_where_the_headers_say() {
        // 3.13: 3 header slots + 3 runtime_state + 13 interpreter_state = 19,
        // so thread_state starts at slot 19 and current_frame is its 5th field.
        assert_eq!(
            offset_of!(DebugOffsetsV313, thread_state.current_frame),
            (19 + 4) * 8
        );
        // interpreter_frame follows thread_state's 9 fields.
        assert_eq!(
            offset_of!(DebugOffsetsV313, interpreter_frame.previous),
            (19 + 9 + 1) * 8
        );
        // 3.14 adds 3 fields to interpreter_state, shifting everything after.
        assert_eq!(
            offset_of!(DebugOffsetsV314, thread_state.current_frame),
            (22 + 4) * 8
        );
        assert_eq!(
            offset_of!(DebugOffsetsV314, interpreter_frame.previous),
            (22 + 9 + 1) * 8
        );
        // And the 3.14-only trailing groups really are at the end.
        assert!(
            offset_of!(DebugOffsetsV314, debugger_support.eval_breaker)
                > offset_of!(DebugOffsetsV314, gc.collecting)
        );
    }

    /// A plausible table, with sizes and offsets taken from a real 3.13
    /// interpreter on aarch64.
    fn good_v313() -> DebugOffsetsV313 {
        // SAFETY: every field is an integer, so all-zero is a valid value.
        let mut t: DebugOffsetsV313 = unsafe { core::mem::zeroed() };
        t.cookie = COOKIE.map(|b| b as c_char);
        t.version = 0x030d_0df0; // 3.13.13
        t.thread_state.size = 312;
        t.thread_state.current_frame = 72;
        t.interpreter_frame.size = 80;
        t.interpreter_frame.previous = 8;
        t.interpreter_frame.executable = 0;
        t.interpreter_frame.instr_ptr = 56;
        t.interpreter_frame.owner = 70;
        t.code_object.size = 208;
        t.code_object.filename = 112;
        t.code_object.name = 120;
        t.code_object.qualname = 128;
        t.code_object.linetable = 136;
        t.code_object.firstlineno = 68;
        t.code_object.co_code_adaptive = 200;
        t.bytes_object.size = 40;
        t.bytes_object.ob_size = 16;
        t.bytes_object.ob_sval = 32;
        t.unicode_object.size = 64;
        t.unicode_object.state = 32;
        t.unicode_object.length = 16;
        t.unicode_object.asciiobject_size = 40;
        t.pyobject.size = 16;
        t.pyobject.ob_type = 8;
        t.type_object.size = 416;
        t.type_object.tp_flags = 168;
        t
    }

    #[test]
    fn accepts_a_plausible_table() {
        let got = good_v313().extract((3, 13)).expect("should validate");
        assert_eq!(got.thread_state_current_frame, 72);
        assert_eq!(got.frame_previous, 8);
        assert_eq!(got.frame_instr_ptr, 56);
        assert_eq!(got.code_linetable, 136);
        assert_eq!(got.unicode_asciiobject_size, 40);
        assert_eq!(got.type_tp_flags, 168);
    }

    #[test]
    fn rejects_a_bad_cookie() {
        let mut t = good_v313();
        t.cookie = b"notpyobj".map(|b| b as c_char);
        assert!(matches!(
            t.extract((3, 13)),
            Err(OffsetsError::BadCookie(_))
        ));
    }

    /// The case that matters in practice: 3.12 has a `_PyRuntime` symbol but
    /// no offsets table, so the cookie is whatever happens to be there. I
    /// confirmed this empirically against a real 3.12 before relying on it.
    #[test]
    fn rejects_an_all_zero_table() {
        // SAFETY: all-zero is a valid value for an all-integer struct.
        let t: DebugOffsetsV313 = unsafe { core::mem::zeroed() };
        assert!(matches!(
            t.extract((3, 13)),
            Err(OffsetsError::BadCookie(_))
        ));
    }

    #[test]
    fn rejects_a_version_mismatch() {
        let mut t = good_v313();
        t.version = 0x030e_05f0; // 3.14.5
        assert_eq!(
            t.extract((3, 13)),
            Err(OffsetsError::VersionMismatch {
                got: (3, 14),
                want: (3, 13)
            })
        );
    }

    #[test]
    fn rejects_a_free_threaded_build() {
        let mut t = good_v313();
        t.free_threaded = 1;
        assert_eq!(t.extract((3, 13)), Err(OffsetsError::FreeThreaded));
    }

    #[test]
    fn rejects_implausible_sizes() {
        for size in [0u64, MAX_PLAUSIBLE_STRUCT_SIZE + 1, u64::MAX] {
            let mut t = good_v313();
            t.interpreter_frame.size = size;
            assert!(
                matches!(
                    t.extract((3, 13)),
                    Err(OffsetsError::ImplausibleSize { .. })
                ),
                "size {size} was accepted"
            );
        }
    }

    #[test]
    fn rejects_an_offset_outside_its_struct() {
        let mut t = good_v313();
        t.interpreter_frame.previous = t.interpreter_frame.size;
        assert!(matches!(
            t.extract((3, 13)),
            Err(OffsetsError::OffsetOutOfRange {
                group: "interpreter_frame",
                field: "previous",
                ..
            })
        ));
    }

    #[test]
    fn reading_a_null_base_is_an_error() {
        // SAFETY: `read` checks for null before dereferencing.
        assert!(unsafe { read(core::ptr::null()) }.is_err());
    }

    /// Reading through a real pointer, which is what resolution does.
    #[cfg(pyroscope_py_minor_13)]
    #[test]
    fn reads_through_a_pointer() {
        let table = good_v313();
        let base: *const u8 = (&raw const table).cast();
        // SAFETY: `base` points at a full, live `DebugOffsetsV313`.
        let got = unsafe { read(base) }.expect("should validate");
        assert_eq!(got.frame_instr_ptr, 56);
    }

    #[test]
    fn the_build_version_is_recorded() {
        assert_eq!(build_major(), 3, "unexpected build-time major version");
        assert!(build_minor() >= 10, "build-time minor looks unset");
        // The mirror selection and the recorded version must agree.
        if let Some((major, minor)) = WANT {
            assert_eq!((major, minor), (build_major(), build_minor()));
        }
    }
}
