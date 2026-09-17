//! Process-wide interned string table.
//!
//! Every native profiler in this extension interns function names and file
//! names here and then refers to them by [`FFIInternedString`], a bare `u32`
//! index into this table. There is exactly one table per process rather than
//! one per profiler, because an index is only meaningful relative to the table
//! that minted it and [`crate::encode::pprof::PProfBuilder`] writes those
//! indices straight into the `string_table` of the emitted pprof.
//!
//! Deliberately *not* behind the `memory` cargo feature. Free-threaded builds
//! omit that feature (see `setup.py`) but still need to intern for the CPU
//! sampler, so this used to be unreachable there when it lived in
//! `crate::memory`.

use crate::encode::pprof::ffi::{FFIInternedString, FFIStringView};
use crate::encode::pprof::{StringID, StringTable};
use lazy_static::lazy_static;
use std::sync::Mutex;

lazy_static! {
    static ref STRING_TABLE: Mutex<StringTable> = Mutex::new(StringTable::new());
}

/// The shared table, for the dump paths that have to materialize it.
///
/// LOCK ORDER: acquire this before any profile-builder lock (such as the one
/// in `crate::memory`), never the reverse. `dump_pprof` needs both at once;
/// interning needs only this one. A CPU dump path written by copying
/// `memory::dump_pprof` must keep the same order.
pub fn string_table() -> &'static Mutex<StringTable> {
    &STRING_TABLE
}

/// Intern `s` and return its index.
///
/// Infallible, and callers must not try to detect failure: null data, a zero
/// length, and a poisoned lock all yield index 0, which is the index of the
/// empty string and therefore a perfectly usable id. This is the one place
/// this differs from Datadog's `intern_string`, which returns
/// `std::optional` because libdatadog's Profiles Dictionary can fail to
/// allocate.
///
/// # Safety
///
/// `s.data[..s.len]` must be valid UTF-8 and must stay alive for the duration
/// of the call. The UTF-8 check is skipped because this runs on the sampling
/// path; the strings come from CPython's interned name and filename objects.
#[unsafe(no_mangle)]
pub extern "C" fn pyroscope_string_table_intern_string(s: FFIStringView) -> FFIInternedString {
    if s.data.is_null() || s.len == 0 {
        return StringID::empty_ffi_string();
    }
    let unsafe_str = unsafe {
        let s = std::slice::from_raw_parts(s.data as *const u8, s.len);
        std::str::from_utf8_unchecked(s)
    };
    // Note the asymmetry with clear() below, which is deliberate and
    // pre-existing: interning does not recover a poisoned lock (it degrades to
    // the empty string for that one frame), clear() does, and the dump path
    // gives up on the whole profile.
    match STRING_TABLE.lock() {
        Ok(mut string_table) => (&string_table.add(unsafe_str)).into(),
        Err(_) => StringID::empty_ffi_string(),
    }
}

/// Discard every interned string, resetting the table to just the empty string
/// at index 0.
///
/// INVARIANT, read this before adding a call site: **no `FFIInternedString`
/// may outlive a call to this function.** It may only run once every profiler
/// that interns here has stopped *and* every cache of interned indices outside
/// Rust has been discarded. An index is not a pointer: a stale one does not
/// crash, it silently resolves to whatever string later lands at that index,
/// mislabelling frames with no way to detect it after the fact. The only
/// legitimate caller is whole-agent teardown, `crate::ffikit::stop_profilers`.
///
/// The memory profiler satisfies the invariant because teardown runs after the
/// allocator hooks are uninstalled with the GIL held, and every hook also runs
/// under the GIL, so no hook can be mid-push and no live traceback still holds
/// an index.
///
/// Without this, a `pyroscope.shutdown()` and reconfigure cycle would grow the
/// table for the lifetime of the process, and because `take_profile_and_reset`
/// copies the *whole* table into every emitted profile, every later payload
/// would carry the dead session's strings.
///
/// If the invariant ever needs mechanical enforcement rather than
/// documentation, the shape is a `static GENERATION: AtomicU32` bumped here and
/// exported over the C ABI, which lets a C++ cache notice the mismatch and
/// self-heal. That turns silent mislabelling into a bounded cache miss, but it
/// is speculative until there is a consumer, so it is deliberately absent.
pub fn clear() {
    let mut st = STRING_TABLE.lock().unwrap_or_else(|e| e.into_inner());
    *st = StringTable::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(s: &str) -> FFIStringView {
        FFIStringView {
            data: s.as_ptr() as *const std::ffi::c_char,
            len: s.len(),
        }
    }

    /// Every interner assertion lives in this one test on purpose.
    ///
    /// The table is a process global shared with every other test in the
    /// binary, and tests run in parallel, so assertions must be
    /// order-independent and `clear()` must never be called here -- doing so
    /// would race any test that depends on an index staying put.
    ///
    /// Deliberately no invalid-UTF-8 case: the entry point documents valid
    /// UTF-8 as a caller obligation and reads it with `from_utf8_unchecked`,
    /// so feeding it garbage is UB by contract and miri would rightly fail.
    #[test]
    fn intern_ffi_entry_point() {
        let a = pyroscope_string_table_intern_string(view("interner::tests::alpha"));
        let b = pyroscope_string_table_intern_string(view("interner::tests::alpha"));
        let c = pyroscope_string_table_intern_string(view("interner::tests::beta"));

        assert_eq!(a.index, b.index, "the same string must intern to one index");
        assert_ne!(
            a.index, c.index,
            "distinct strings must get distinct indices"
        );
        assert_ne!(
            a.index, 0,
            "a non-empty string must never collide with \"\""
        );

        // Every failure mode yields index 0, the id of the empty string.
        assert_eq!(pyroscope_string_table_intern_string(view("")).index, 0);
        assert_eq!(
            pyroscope_string_table_intern_string(FFIStringView {
                data: std::ptr::null(),
                len: 0,
            })
            .index,
            0
        );
        assert_eq!(
            pyroscope_string_table_intern_string(FFIStringView {
                data: view("nonempty").data,
                len: 0,
            })
            .index,
            0
        );
    }
}
