//! CPython 3.11 and later's co_linetable location entries:
//! https://github.com/python/cpython/blob/v3.14.0/Objects/locations.md
//! https://github.com/python/cpython/blob/v3.14.0/Objects/codeobject.c

use super::{Error, Table};

pub(super) fn line_number(mut table: Table, first: i32, offset: i32) -> Result<i32, Error> {
    let mut line = first;
    let mut end = 0i32;
    while table.remaining != 0 {
        let header = table.next()?;
        if header & 0x80 == 0 {
            return Err(Error::LineTable);
        }
        let code = (header >> 3) & 15;
        let delta = match code {
            0..=9 => {
                payload(&mut table)?;
                0
            }
            10..=12 => {
                payload(&mut table)?;
                payload(&mut table)?;
                i32::from(code - 10)
            }
            13 | 14 => {
                let encoded = varint(&mut table)?;
                let magnitude = (encoded >> 1) as i32;
                let delta = if encoded & 1 == 0 {
                    magnitude
                } else {
                    -magnitude
                };
                if code == 14 {
                    // End line delta and both columns do not affect the start line.
                    varint(&mut table)?;
                    varint(&mut table)?;
                    varint(&mut table)?;
                }
                delta
            }
            15 => 0,
            _ => unreachable!(),
        };
        if table.peek()?.is_some_and(|byte| byte & 0x80 == 0) {
            return Err(Error::LineTable);
        }
        line = line.checked_add(delta).ok_or(Error::LineTable)?;
        end = end
            .checked_add((i32::from(header & 7) + 1) * 2)
            .ok_or(Error::LineTable)?;
        if offset < end {
            return Ok(if code == 15 { -1 } else { line });
        }
    }
    Err(Error::LineTable)
}

fn payload(table: &mut Table) -> Result<u8, Error> {
    let byte = table.next()?;
    if byte & 0x80 != 0 {
        return Err(Error::LineTable);
    }
    Ok(byte)
}

fn varint(table: &mut Table) -> Result<u32, Error> {
    let mut value = 0u32;
    for shift in (0..32).step_by(6) {
        let byte = payload(table)?;
        let part = u32::from(byte & 63);
        if part > u32::MAX >> shift {
            return Err(Error::LineTable);
        }
        value |= part << shift;
        if byte & 64 == 0 {
            return Ok(value);
        }
    }
    Err(Error::LineTable)
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
    fn locations_cover_all_forms_and_preserve_line_across_missing_locations() {
        let table = [
            0x87, 0x01, // short, eight units, unchanged
            0xd0, 1, 2, // one line, delta 0
            0xd8, 1, 2, // one line, delta 1
            0xe0, 1, 2, // one line, delta 2
            0xe8, 0x49, 0x03, // no columns, delta -100
            0xf0, 0x48, 0x03, 1, 2, 3,    // long, delta +100
            0xf8, // no location
            0x80, 0x01, // short after no location
        ];
        let expected: Vec<_> = [10; 16]
            .into_iter()
            .chain([10, 10, 11, 11, 13, 13, -87, -87, 13, 13, -1, -1, 13, 13])
            .collect();
        for (offset, line) in expected.into_iter().enumerate() {
            assert_eq!(
                decode(&table, 10, offset as i32).unwrap(),
                line,
                "offset {offset}"
            );
        }
        assert!(decode(&table, 10, 30).is_err());
    }

    #[test]
    fn malformed_locations_fail_without_panics_or_unbounded_varints() {
        for table in [
            &[][..],
            &[0],
            &[0x80],
            &[0x80, 0x80],
            &[0x80, 0, 0],
            &[0xd0, 0],
            &[0xe8, 0x40],
            &[0xf0, 0, 0, 0],
            &[0xe8, 0x7f, 0x7f, 0x7f, 0x7f, 0x7f, 0x04],
            &[0xe8, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0],
        ] {
            assert!(decode(table, 10, 0).is_err(), "{table:?}");
        }
        assert!(decode(&[0xe8, 2], i32::MAX, 0).is_err());
        assert!(decode(&[0xe8, 3], i32::MIN, 0).is_err());
    }
}
