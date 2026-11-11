//! Reads CPython Unicode objects and decodes their canonical character storage.

use super::{Error, Types, check_type, copy_bytes, layout, read_u64, word};

// Bound allocations and decoder work for reused addresses or torn reads.
const MAX_STRING_BYTES: usize = 1 << 20;

pub(super) fn read<T: Types>(address: usize) -> Result<String, Error> {
    // Include/cpython/unicodeobject.h: PyUnicode_DATA / PyUnicode_KIND, PEP 393.
    // https://github.com/python/cpython/blob/v3.14.0/Include/cpython/unicodeobject.h
    check_type(address, T::string())?;
    let len = word(address, layout::UNICODE_LENGTH)?;
    let state = read_u64(address.wrapping_add(layout::UNICODE_STATE))?;
    let kind = ((state & layout::STATE_KIND_MASK) >> layout::STATE_KIND_SHIFT) as usize;
    let ascii = state & layout::STATE_ASCII != 0;
    if !matches!(kind, 1 | 2 | 4) || (ascii && kind != 1) {
        return Err(Error::Unicode);
    }
    let data = if state & layout::STATE_COMPACT != 0 {
        address.wrapping_add(if ascii {
            layout::ASCII_SIZE
        } else {
            layout::COMPACT_SIZE
        })
    } else {
        // PyUnicodeObject starts with PyCompactUnicodeObject, then the data pointer union.
        word(address, layout::COMPACT_SIZE)?
    };
    let size = len.checked_mul(kind).ok_or(Error::Length)?;
    let bytes = copy_bytes(data, size, MAX_STRING_BYTES)?;
    decode_unicode(&bytes, kind, ascii)
}

fn decode_unicode(bytes: &[u8], kind: usize, ascii: bool) -> Result<String, Error> {
    if !matches!(kind, 1 | 2 | 4) || !bytes.len().is_multiple_of(kind) || (ascii && kind != 1) {
        return Err(Error::Unicode);
    }
    let mut result = String::with_capacity(bytes.len());
    for unit in bytes.chunks_exact(kind) {
        let codepoint = match kind {
            1 => u32::from(unit[0]),
            2 => u32::from(u16::from_ne_bytes(unit.try_into().unwrap())),
            4 => u32::from_ne_bytes(unit.try_into().unwrap()),
            _ => unreachable!(),
        };
        if codepoint > 0x10ffff || (ascii && codepoint > 0x7f) {
            return Err(Error::Unicode);
        }
        // Python permits lone surrogates. Rust strings do not; replace each surrogate.
        // UCS2 data is a sequence of code points, not UTF-16 surrogate pairs.
        result.push(char::from_u32(codepoint).unwrap_or(char::REPLACEMENT_CHARACTER));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    #[cfg(not(miri))]
    use super::super::tests::{TestTypes, put_word, recovery, string_object};
    use super::*;

    #[test]
    fn decodes_python_code_points() {
        assert_eq!(
            decode_unicode(b"module\0name", 1, true).unwrap(),
            "module\0name"
        );
        assert_eq!(decode_unicode(&[0xe9, 0xff], 1, false).unwrap(), "éÿ");
        let ucs2: Vec<_> = [0x4e2du16, 0xd800, 0xdc00]
            .into_iter()
            .flat_map(u16::to_ne_bytes)
            .collect();
        assert_eq!(decode_unicode(&ucs2, 2, false).unwrap(), "中��");
        let ucs4: Vec<_> = [0x1f40du32, 0xd800]
            .into_iter()
            .flat_map(u32::to_ne_bytes)
            .collect();
        assert_eq!(decode_unicode(&ucs4, 4, false).unwrap(), "🐍�");
        for (bytes, kind, ascii) in [
            (vec![0xff], 1, true),
            (vec![0], 2, false),
            (vec![], 3, false),
            (vec![], 2, true),
            (0x110000u32.to_ne_bytes().to_vec(), 4, false),
        ] {
            assert!(matches!(
                decode_unicode(&bytes, kind, ascii),
                Err(Error::Unicode)
            ));
        }
    }

    #[cfg(not(miri))]
    #[test]
    fn reads_compact_and_noncompact_unicode_with_bounds_and_types() {
        recovery();
        for (bytes, kind, ascii, expected) in [
            (b"hello".to_vec(), 1, true, "hello"),
            (vec![0xe9], 1, false, "é"),
            (0x4e2du16.to_ne_bytes().to_vec(), 2, false, "中"),
            (0x1f40du32.to_ne_bytes().to_vec(), 4, false, "🐍"),
        ] {
            let mut object = string_object(&bytes, kind, ascii);
            assert_eq!(
                read::<TestTypes>(object.as_ptr() as usize).unwrap(),
                expected
            );
            // The noncompact representation has an external canonical data buffer.
            object.resize(layout::COMPACT_SIZE + 8, 0);
            let state = word(object.as_ptr() as usize, layout::UNICODE_STATE).unwrap();
            put_word(
                &mut object,
                layout::UNICODE_STATE,
                state & !(layout::STATE_COMPACT as usize),
            );
            put_word(&mut object, layout::COMPACT_SIZE, bytes.as_ptr() as usize);
            assert_eq!(
                read::<TestTypes>(object.as_ptr() as usize).unwrap(),
                expected
            );
            put_word(&mut object, layout::UNICODE_LENGTH, usize::MAX);
            assert!(matches!(
                read::<TestTypes>(object.as_ptr() as usize),
                Err(Error::Length)
            ));
            put_word(&mut object, layout::OB_TYPE, 999);
            assert!(matches!(
                read::<TestTypes>(object.as_ptr() as usize),
                Err(Error::ObjectType(_))
            ));
        }
    }
}
