//! CPython 3.10's co_linetable address ranges:
//! https://github.com/python/cpython/blob/v3.10.14/Objects/lnotab_notes.txt
//! https://github.com/python/cpython/blob/v3.10.14/Objects/codeobject.c

use super::{Error, Table};

pub(super) fn line_number(mut table: Table, first: i32, offset: i32) -> Result<i32, Error> {
    if !table.remaining.is_multiple_of(2) {
        return Err(Error::LineTable);
    }
    let mut line = first;
    let mut end = 0i32;
    while table.remaining != 0 {
        end = end
            .checked_add(i32::from(table.next()?))
            .ok_or(Error::LineTable)?;
        let delta = i32::from(table.next()? as i8);
        if delta != -128 {
            line = line.checked_add(delta).ok_or(Error::LineTable)?;
        }
        if offset < end {
            return Ok(if delta == -128 { -1 } else { line });
        }
    }
    Ok(-1)
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;

    fn decode(table: &[u8], first: i32, offset: i32) -> Result<i32, Error> {
        super::super::super::tests::recovery();
        line_number(
            Table::new(table.as_ptr() as usize, table.len())?,
            first,
            offset,
        )
    }

    #[test]
    fn empty_table_has_no_line_mapping() {
        for offset in [0, 2, i32::MAX] {
            assert_eq!(decode(&[], 10, offset).unwrap(), -1);
        }
    }

    #[test]
    fn ranges_310_include_empty_ranges_signed_deltas_and_missing_lines() {
        let table = [2, 0, 0, 127, 2, 1, 2, 128, 2, 255, 2, 0];
        let expected = [10, 10, 138, 138, -1, -1, 137, 137, 137, 137];
        for (offset, line) in expected.into_iter().enumerate() {
            assert_eq!(decode(&table, 10, offset as i32).unwrap(), line);
        }
        assert_eq!(decode(&table, 10, 10).unwrap(), -1);
        assert!(decode(&[1], 10, 0).is_err());
        assert!(decode(&[2, 1], i32::MAX, 0).is_err());
    }
}
