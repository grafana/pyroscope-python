use crate::encode::pprof::ffi::{FFIInternedString, FFIStringView};
use crate::encode::pprof::{StringID, StringTable};
use crate::forksafety::LeakableMutex;
use std::sync::Mutex;

static STRING_TABLE: LeakableMutex<StringTable> = LeakableMutex::new();

/// The shared table, for the dump paths that have to materialize it.
///
/// LOCK ORDER: acquire this before any profile-builder lock (such as the one
/// in `crate::memory`), never the reverse. `dump_pprof` needs both at once;
/// interning needs only this one. A CPU dump path written by copying
/// `memory::dump_pprof` must keep the same order.
pub fn string_table() -> &'static Mutex<StringTable> {
    STRING_TABLE.mutex()
}

#[unsafe(no_mangle)]
pub extern "C" fn pyroscope_string_table_intern_utf8(s: FFIStringView) -> FFIInternedString {
    if s.data.is_null() || s.len == 0 {
        return StringID::empty_ffi_string();
    }
    let bytes = unsafe { std::slice::from_raw_parts(s.data as *const u8, s.len) };
    let s = std::str::from_utf8(bytes).unwrap_or("<non-utf8>");
    match STRING_TABLE.mutex().lock() {
        Ok(mut string_table) => (&string_table.add(s)).into(),
        Err(_) => StringID::empty_ffi_string(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn pyroscope_string_table_intern_ascii(s: FFIStringView) -> FFIInternedString {
    if s.data.is_null() || s.len == 0 {
        return StringID::empty_ffi_string();
    }
    let bytes = unsafe { std::slice::from_raw_parts(s.data as *const u8, s.len) };
    let s = unsafe { std::str::from_utf8_unchecked(bytes) };
    match STRING_TABLE.mutex().lock() {
        Ok(mut string_table) => (&string_table.add(s)).into(),
        Err(_) => StringID::empty_ffi_string(),
    }
}

pub fn clear() {
    let mut st = STRING_TABLE.mutex().lock().unwrap_or_else(|e| e.into_inner());
    *st = StringTable::new();
}

pub fn postfork_child() {
    #[cfg(not(miri))]
    STRING_TABLE.leak_and_reset();
    #[cfg(miri)]
    let _ = STRING_TABLE.leak_and_reset();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(s: &str) -> FFIStringView {
        view_bytes(s.as_bytes())
    }

    fn view_bytes(b: &[u8]) -> FFIStringView {
        FFIStringView {
            data: b.as_ptr() as *const std::ffi::c_char,
            len: b.len(),
        }
    }

    #[test]
    fn intern_ffi_entry_point() {
        let a = pyroscope_string_table_intern_utf8(view("interner::tests::alpha"));
        let b = pyroscope_string_table_intern_utf8(view("interner::tests::alpha"));
        let c = pyroscope_string_table_intern_utf8(view("interner::tests::beta"));

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
        assert_eq!(pyroscope_string_table_intern_utf8(view("")).index, 0);
        assert_eq!(
            pyroscope_string_table_intern_utf8(FFIStringView {
                data: std::ptr::null(),
                len: 0,
            })
            .index,
            0
        );
        assert_eq!(
            pyroscope_string_table_intern_utf8(FFIStringView {
                data: view("nonempty").data,
                len: 0,
            })
            .index,
            0
        );
    }

    #[test]
    fn intern_ascii_agrees_with_the_checked_door() {
        let name = "interner::tests::ascii_door";
        let checked = pyroscope_string_table_intern_utf8(view(name));
        let ascii = pyroscope_string_table_intern_ascii(view(name));

        assert_eq!(checked.index, ascii.index);
        assert_ne!(checked.index, 0);

        assert_eq!(pyroscope_string_table_intern_ascii(view("")).index, 0);
        assert_eq!(
            pyroscope_string_table_intern_ascii(FFIStringView {
                data: std::ptr::null(),
                len: 0,
            })
            .index,
            0
        );
    }

    #[test]
    fn intern_sanitizes_invalid_utf8() {
        let latin1 = pyroscope_string_table_intern_utf8(view_bytes(b"interner::caf\xe9"));
        let lone = pyroscope_string_table_intern_utf8(view_bytes(b"interner::\xe9"));

        let sentinel = pyroscope_string_table_intern_utf8(view("<non-utf8>"));
        assert_ne!(sentinel.index, 0);
        assert_eq!(latin1.index, sentinel.index);
        assert_eq!(lone.index, sentinel.index);
        assert_ne!(
            sentinel.index,
            pyroscope_string_table_intern_utf8(view("interner::caf\u{e9}")).index,
            "valid UTF-8 must be stored as itself"
        );
    }
}
