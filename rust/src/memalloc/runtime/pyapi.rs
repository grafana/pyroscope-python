//! Locating CPython's internals at run time.
//!
//! The only thing the profiler cannot get from `pyo3-ffi` is `_PyRuntime`,
//! which is a data symbol rather than a declared API. It is resolved with
//! `dlsym` rather than a link-time `extern` so that a Python without it
//! produces a clean error at `start()` instead of failing to load the
//! extension at import time.
//!
//! Resolution happens once, from `start()`, with the GIL held and before any
//! allocator hook is installed. Nothing here is called from the hook path.

use crate::memalloc::pure::frames::TypeAddrs;
use crate::memalloc::pure::offsets::{self, Offsets, OffsetsError};
use std::ffi::c_void;
use std::sync::OnceLock;

/// The symbol holding `_PyRuntimeState`, whose first member is the offsets
/// table.
const PY_RUNTIME_SYMBOL: &[u8] = b"_PyRuntime\0";

/// Type objects the frame walk compares against, rather than calling
/// `PyCode_Check` / `PyUnicode_Check`, which are macros over the same
/// comparison.
const PY_CODE_TYPE_SYMBOL: &[u8] = b"PyCode_Type\0";
const PY_UNICODE_TYPE_SYMBOL: &[u8] = b"PyUnicode_Type\0";

/// Returns the current thread state without the fatal-error-on-null behaviour
/// of `PyThreadState_Get`. Public API since 3.13; the underscore-prefixed name
/// is the older spelling, kept as a fallback.
const TSTATE_SYMBOLS: &[&[u8]] = &[
    b"PyThreadState_GetUnchecked\0",
    b"_PyThreadState_UncheckedGet\0",
];

/// Signature of `PyThreadState_GetUnchecked`.
type TStateGet = unsafe extern "C" fn() -> *mut c_void;

/// Cached result of [`resolve`]. Resolution is idempotent and its inputs
/// cannot change within a process, so it is computed at most once.
static OFFSETS: OnceLock<Result<Offsets, OffsetsError>> = OnceLock::new();

/// Look up a symbol in the global scope.
///
/// `RTLD_DEFAULT` searches the main executable and everything already loaded,
/// which covers both a shared libpython and a statically linked interpreter
/// that exports its symbols (as CPython's own build does).
fn lookup(symbol: &[u8]) -> *mut c_void {
    debug_assert_eq!(
        symbol.last().copied(),
        Some(0),
        "dlsym needs a NUL-terminated name"
    );
    // SAFETY: `symbol` is NUL terminated (asserted above) and `RTLD_DEFAULT`
    // is a valid handle. `dlsym` returns null when the symbol is absent.
    unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr().cast()) }
}

/// Address of `_PyRuntime`, or null if this interpreter does not export it.
pub fn py_runtime() -> *const u8 {
    lookup(PY_RUNTIME_SYMBOL).cast_const().cast()
}

/// Cached addresses of `PyCode_Type` and `PyUnicode_Type`.
static TYPES: OnceLock<TypeAddrs> = OnceLock::new();

/// Cached `PyThreadState_GetUnchecked`, if this interpreter has it.
static TSTATE_GET: OnceLock<Option<TStateGet>> = OnceLock::new();

/// Resolve the type objects the frame walk compares against.
///
/// A zero address means the symbol was missing, which makes the walk reject
/// every frame rather than decode something unchecked.
pub fn type_addrs() -> TypeAddrs {
    *TYPES.get_or_init(|| TypeAddrs {
        code: lookup(PY_CODE_TYPE_SYMBOL) as usize,
        unicode: lookup(PY_UNICODE_TYPE_SYMBOL) as usize,
    })
}

/// The current thread state, or 0 if it cannot be determined.
///
/// Must only be called with the GIL held.
pub fn current_thread_state() -> usize {
    let getter = *TSTATE_GET.get_or_init(|| {
        TSTATE_SYMBOLS.iter().find_map(|symbol| {
            let address = lookup(symbol);
            if address.is_null() {
                return None;
            }
            // SAFETY: both symbols name a `PyThreadState *(void)` function in
            // libpython, so this is the correct signature for either.
            Some(unsafe { std::mem::transmute::<*mut c_void, TStateGet>(address) })
        })
    });
    match getter {
        // SAFETY: the caller holds the GIL, which is this function's
        // documented requirement.
        Some(get) => unsafe { get() as usize },
        None => 0,
    }
}

/// Resolve and validate the debug-offsets table for this interpreter.
///
/// Errors are not fatal: the caller declines to install the allocator hooks
/// and memory profiling degrades to a logged warning.
pub fn resolve() -> Result<Offsets, OffsetsError> {
    *OFFSETS.get_or_init(|| {
        // SAFETY: `py_runtime` returns either null, which `read` checks for,
        // or the address of `_PyRuntime`, whose first member is the offsets
        // table. Only the interpreter we are loaded into can own that symbol.
        unsafe { offsets::read(py_runtime()) }
    })
}

/// Human-readable description of a resolution failure, for logs and for the
/// diagnostic entry point.
pub fn describe(error: &OffsetsError) -> String {
    match *error {
        OffsetsError::UnsupportedVersion { major, minor } => {
            format!("memory profiling has no CPython {major}.{minor} support in this build")
        }
        OffsetsError::SymbolMissing => "this interpreter does not export _PyRuntime".to_owned(),
        OffsetsError::BadCookie(cookie) => format!(
            "_PyRuntime does not start with a _Py_DebugOffsets table \
             (cookie {cookie:?}); it was added in CPython 3.13"
        ),
        OffsetsError::VersionMismatch { got, want } => format!(
            "this extension was built for CPython {}.{} but is running on {}.{}",
            want.0, want.1, got.0, got.1
        ),
        OffsetsError::FreeThreaded => {
            "free-threaded CPython is not supported by the memory profiler".to_owned()
        }
        OffsetsError::ImplausibleSize { group, size } => {
            format!("_Py_DebugOffsets reports an implausible sizeof({group}) of {size}")
        }
        OffsetsError::OffsetOutOfRange {
            group,
            field,
            offset,
            size,
        } => format!(
            "_Py_DebugOffsets puts {group}.{field} at {offset}, \
             outside its own {size}-byte struct"
        ),
    }
}

/// Flatten the resolved offsets into `(field, value)` pairs.
///
/// Used by the diagnostic entry point so `scripts/check_debug_offsets.py` can
/// compare what Rust believes against what ctypes reads from the same
/// interpreter, field by field.
pub fn report(offsets: &Offsets) -> Vec<(&'static str, usize)> {
    vec![
        ("thread_state_size", offsets.thread_state_size),
        (
            "thread_state_current_frame",
            offsets.thread_state_current_frame,
        ),
        ("frame_size", offsets.frame_size),
        ("frame_previous", offsets.frame_previous),
        ("frame_executable", offsets.frame_executable),
        ("frame_instr_ptr", offsets.frame_instr_ptr),
        ("frame_owner", offsets.frame_owner),
        ("code_size", offsets.code_size),
        ("code_filename", offsets.code_filename),
        ("code_name", offsets.code_name),
        ("code_qualname", offsets.code_qualname),
        ("code_linetable", offsets.code_linetable),
        ("code_firstlineno", offsets.code_firstlineno),
        ("code_co_code_adaptive", offsets.code_co_code_adaptive),
        ("bytes_ob_size", offsets.bytes_ob_size),
        ("bytes_ob_sval", offsets.bytes_ob_sval),
        ("unicode_size", offsets.unicode_size),
        ("unicode_state", offsets.unicode_state),
        ("unicode_length", offsets.unicode_length),
        ("unicode_asciiobject_size", offsets.unicode_asciiobject_size),
        ("pyobject_ob_type", offsets.pyobject_ob_type),
        ("type_tp_flags", offsets.type_tp_flags),
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::integer_division)]

    use super::*;
    use std::mem::size_of;

    /// Type resolution must be total and cached, whatever the test binary is
    /// linked against (usually no libpython at all).
    #[test]
    fn type_resolution_is_total_and_cached() {
        assert_eq!(type_addrs(), type_addrs());
    }

    /// With no interpreter in the test binary there is no thread state, and
    /// asking for one must return 0 rather than crashing.
    #[test]
    fn asking_for_a_thread_state_is_safe() {
        let _ = current_thread_state();
    }

    #[test]
    fn looking_up_a_missing_symbol_returns_null() {
        assert!(lookup(b"pyroscope_definitely_not_a_real_symbol\0").is_null());
    }

    /// `dlsym` with `RTLD_DEFAULT` must find a symbol from libc, which proves
    /// the lookup mechanism works in whatever binary the tests run in.
    #[test]
    fn looking_up_a_present_symbol_succeeds() {
        assert!(!lookup(b"malloc\0").is_null());
    }

    /// Resolution must be total: it either yields offsets or an error, and
    /// never panics, whichever interpreter the test binary happens to be
    /// linked against (usually none at all).
    #[test]
    fn resolve_is_total_and_cached() {
        let first = resolve();
        let second = resolve();
        assert_eq!(first, second, "resolution is not idempotent");
        if let Err(error) = first {
            assert!(!describe(&error).is_empty());
        }
    }

    /// The report must cover every field of `Offsets`, or the cross-check
    /// script would silently skip one.
    #[test]
    fn the_report_covers_every_offset_field() {
        let offsets = Offsets::default();
        let pairs = report(&offsets);
        // Offsets is a flat struct of usize fields; compare against its size.
        let field_count = size_of::<Offsets>() / size_of::<usize>();
        assert_eq!(
            pairs.len(),
            field_count,
            "report() lists {} of {field_count} Offsets fields",
            pairs.len()
        );
        let mut names: Vec<&str> = pairs.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "report() has duplicate field names");
    }

    #[test]
    fn every_error_has_a_description() {
        let errors = [
            OffsetsError::UnsupportedVersion {
                major: 3,
                minor: 15,
            },
            OffsetsError::SymbolMissing,
            OffsetsError::BadCookie([0; 8]),
            OffsetsError::VersionMismatch {
                got: (3, 14),
                want: (3, 13),
            },
            OffsetsError::FreeThreaded,
            OffsetsError::ImplausibleSize {
                group: "thread_state",
                size: 0,
            },
            OffsetsError::OffsetOutOfRange {
                group: "code_object",
                field: "linetable",
                offset: 4096,
                size: 208,
            },
        ];
        for error in errors {
            let text = describe(&error);
            assert!(!text.is_empty(), "{error:?} has no description");
            assert!(text.is_ascii(), "{error:?} description is not plain text");
        }
    }
}
