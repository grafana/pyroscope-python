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

#[unsafe(no_mangle)]
pub extern "C" fn pyroscope_string_table_intern_string(s: FFIStringView) -> FFIInternedString {
    if s.data.is_null() || s.len == 0 {
        return StringID::empty_ffi_string();
    }
    let unsafe_str = unsafe {
        let s = std::slice::from_raw_parts(s.data as *const u8, s.len);
        std::str::from_utf8_unchecked(s)
    };
    match STRING_TABLE.lock() {
        Ok(mut string_table) => (&string_table.add(unsafe_str)).into(),
        Err(_) => StringID::empty_ffi_string(),
    }
}

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
