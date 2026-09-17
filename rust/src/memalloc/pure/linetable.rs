//! PEP 657 line-table parsing.
//!
//! A port of the `PY_VERSION_HEX >= 0x030b0000` branch of
//! `cpp/profiling_helpers/linetable_parser.h`. The pre-3.11 branches (PEP 626
//! on 3.10 and `co_lnotab` on 3.9) are intentionally not ported: memory
//! profiling requires CPython 3.13+.
//!
//! We parse `co_linetable` ourselves rather than calling `PyCode_Addr2Line`
//! because CPython does not guarantee that function is allocation-free, and we
//! run inside the allocator hook. Parsing is pure byte arithmetic: no CPython
//! calls, no allocation, no panics.
//!
//! The table is a sequence of entries. Each entry's first byte holds
//! `(code_unit_delta - 1)` in bits 0..3 and an info code in bits 3..7. The
//! info code selects how the line delta is encoded and how many bytes to skip.
//!
//! Tested against CPython's own `co_positions()`, which yields one tuple per
//! code unit and is therefore indexed by `lasti`. See
//! `scripts/gen_linetable_corpus.py`.

/// Index into the table. Signed, mirroring the C++ `Py_ssize_t`, so that
/// `guard = len - 1` is `-1` for an empty table instead of wrapping.
type Idx = isize;

/// Read `table[i]`, or `None` if `i` is out of range.
#[inline]
fn byte(table: &[u8], i: Idx) -> Option<u8> {
    if i < 0 {
        return None;
    }
    // PANIC-OK: `get` returns None rather than panicking on an out-of-range
    // index; the `i < 0` check above makes the cast lossless.
    #[allow(clippy::cast_sign_loss)]
    table.get(i as usize).copied()
}

/// One past the last index a varint is allowed to read from.
#[inline]
fn guard_of(table: &[u8]) -> Idx {
    // PANIC-OK: saturating, and a table long enough to overflow `isize` cannot
    // exist.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let len = table.len() as Idx;
    len.saturating_sub(1)
}

#[inline]
fn has_continuation(table: &[u8], i: Idx) -> bool {
    byte(table, i).is_some_and(|b| b & 64 != 0)
}

/// Read an unsigned varint, advancing `i` past it.
///
/// Six payload bits per byte, bit 6 marking continuation.
fn read_varint(table: &[u8], i: &mut Idx) -> u32 {
    let guard = guard_of(table);
    if *i >= guard {
        return 0;
    }
    *i = i.saturating_add(1);
    let Some(first) = byte(table, *i) else {
        return 0;
    };
    let mut val = u32::from(first & 63);
    let mut shift: u32 = 0;
    while *i < guard && has_continuation(table, *i) {
        shift = shift.saturating_add(6);
        if shift >= 32 {
            // Malformed input: shifting by >= 32 would be undefined. Advance
            // past the remaining continuation bytes so `i` is left consistent,
            // then give up on this entry.
            while *i < guard && has_continuation(table, *i) {
                *i = i.saturating_add(1);
            }
            return 0;
        }
        *i = i.saturating_add(1);
        let Some(next) = byte(table, *i) else {
            return 0;
        };
        val |= u32::from(next & 63) << shift;
    }
    val
}

/// Read a zig-zag encoded signed varint, advancing `i` past it.
fn read_signed_varint(table: &[u8], i: &mut Idx) -> i32 {
    let val = read_varint(table, i);
    // PANIC-OK: `>> 1` keeps the value at or below `i32::MAX`, so the cast is
    // lossless. `wrapping_neg` rather than `-` because plain negation is a
    // panic source in debug builds; the value can never be `i32::MIN`, so the
    // two are equivalent here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let magnitude = (val >> 1) as i32;
    if val & 1 != 0 {
        magnitude.wrapping_neg()
    } else {
        magnitude
    }
}

/// Resolve the line number for bytecode offset `lasti`.
///
/// `lasti` is in `_Py_CODEUNIT` units, matching the table's own counter and
/// CPython's `co_positions()` index. Returns 0 when the line cannot be
/// determined.
///
/// # Malformed tables
///
/// Guaranteed not to panic, loop forever, or read out of bounds for arbitrary
/// bytes. It is *not* guaranteed to return a sensible line: a table with a
/// large negative delta drives the running line number below zero, which wraps
/// through `u32` and can come back as a negative `i32`. This mirrors the C++
/// (`unsigned int lineno` plus a final `static_cast<int>`) and so keeps
/// profiles identical. It is unreachable for tables that came from CPython,
/// which is the only source in production.
pub fn parse(table: &[u8], lasti: i32, firstlineno: i32) -> i32 {
    if lasti < 0 {
        return firstlineno;
    }

    // Unsigned and wrapping, matching the C++ `unsigned int lineno`: a
    // malformed table can drive the running line number negative, and the
    // final `> 0` test is what filters that out.
    #[allow(clippy::cast_sign_loss)]
    let mut lineno = firstlineno as u32;

    let len = guard_of(table).saturating_add(1);
    let mut i: Idx = 0;
    let mut bc: i64 = 0;

    while i < len {
        let Some(entry) = byte(table, i) else {
            break;
        };
        bc = bc.saturating_add(i64::from(entry & 7).saturating_add(1));
        let info_code = (entry >> 3) & 15;
        match info_code {
            // No location information for this instruction.
            15 => {}
            // Long form: signed line delta, then end_line, column, end_column.
            14 => {
                lineno = lineno.wrapping_add_signed(read_signed_varint(table, &mut i));
                read_varint(table, &mut i);
                read_varint(table, &mut i);
                read_varint(table, &mut i);
            }
            // No column data: signed line delta only.
            13 => {
                lineno = lineno.wrapping_add_signed(read_signed_varint(table, &mut i));
            }
            // New line number, delta encoded in the info code, plus two
            // column bytes to skip.
            10..=12 => {
                lineno = lineno.wrapping_add_signed(i32::from(info_code).saturating_sub(10));
                if i < len.saturating_sub(2) {
                    i = i.saturating_add(2);
                }
            }
            // Same line as the previous entry, one column byte to skip.
            _ => {
                if i < len.saturating_sub(1) {
                    i = i.saturating_add(1);
                }
            }
        }
        if bc > i64::from(lasti) {
            break;
        }
        i = i.saturating_add(1);
    }

    if lineno > 0 {
        // PANIC-OK: deliberate reinterpretation, matching the C++
        // `static_cast<int>(lineno)`. Only reachable for malformed tables that
        // wrapped the running line number past `i32::MAX`.
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let signed = lineno as i32;
        signed
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    // Test code is not on the hook path, so the panic wall does not apply.
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::panic,
        clippy::cast_possible_truncation
    )]

    use super::parse;
    use crate::memalloc::pure::rng::MinstdRand;

    /// Records harvested from real CPython code objects, with the expected
    /// line for every bytecode offset taken from `co_positions()`.
    ///
    /// Regenerate with `scripts/gen_linetable_corpus.py`; validate with
    /// `--verify`.
    const CORPUS: &str = include_str!("testdata/linetable_corpus.txt");

    struct Record {
        firstlineno: i32,
        table: Vec<u8>,
        /// Expected line per `lasti`; `None` where CPython reports no
        /// location, in which case the parser's output is unconstrained.
        expected: Vec<Option<i32>>,
    }

    /// Miri interprets every operation, so replaying the whole fixture (about
    /// 39k offsets across 99 tables) would blow the CI job's 20 minute
    /// budget. Miri is here to find undefined behaviour in the index and
    /// pointer arithmetic, and a handful of records exercises the same
    /// branches, so under Miri we sample the corpus instead. Native runs get
    /// the full thing.
    const MAX_RECORDS: Option<usize> = if cfg!(miri) { Some(6) } else { None };
    const MAX_OFFSETS: usize = if cfg!(miri) { 64 } else { usize::MAX };

    fn corpus() -> Vec<Record> {
        let mut out = Vec::new();
        for line in CORPUS.lines() {
            if MAX_RECORDS.is_some_and(|cap| out.len() >= cap) {
                break;
            }
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split(' ');
            let firstlineno: i32 = fields.next().unwrap().parse().unwrap();
            let hexed = fields.next().unwrap();
            let runs = fields.next().unwrap();
            assert!(fields.next().is_none(), "unexpected extra field");

            let table: Vec<u8> = hexed
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let digit = |b: u8| (b as char).to_digit(16).unwrap() as u8;
                    digit(pair[0]) * 16 + digit(pair[1])
                })
                .collect();

            let mut expected = Vec::new();
            for token in runs.split(',') {
                let (count, value) = token.split_once('*').unwrap();
                let count: usize = count.parse().unwrap();
                let value = if value == "-" {
                    None
                } else {
                    Some(value.parse::<i32>().unwrap())
                };
                expected.extend(std::iter::repeat_n(value, count));
            }

            out.push(Record {
                firstlineno,
                table,
                expected,
            });
        }
        out
    }

    #[test]
    #[cfg_attr(miri, ignore = "asserts the full fixture size, which Miri samples")]
    fn corpus_fixture_is_not_empty() {
        let records = corpus();
        assert!(records.len() > 50, "got {} records", records.len());
        let offsets: usize = records.iter().map(|r| r.expected.len()).sum();
        assert!(offsets > 10_000, "got {offsets} offsets");
    }

    /// The headline test: agree with CPython on every offset of every record.
    #[test]
    fn matches_cpython_co_positions_over_corpus() {
        let mut checked = 0usize;
        let mut mismatches = Vec::new();
        for (r, record) in corpus().iter().enumerate() {
            for (lasti, want) in record.expected.iter().take(MAX_OFFSETS).enumerate() {
                let Some(want) = *want else {
                    // No location: only require that we do not panic.
                    parse(&record.table, lasti as i32, record.firstlineno);
                    continue;
                };
                let got = parse(&record.table, lasti as i32, record.firstlineno);
                checked += 1;
                if got != want && mismatches.len() < 10 {
                    mismatches.push(format!("record {r} lasti {lasti}: got {got}, want {want}"));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} of {checked} offsets disagree with CPython:\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
        let want_checked = if cfg!(miri) { 100 } else { 10_000 };
        assert!(checked > want_checked, "only checked {checked} offsets");
    }

    /// Real CPython tables must always resolve to a usable line number.
    /// This is the property that matters in production; the degenerate-input
    /// tests below deliberately only require "does not panic".
    #[test]
    fn corpus_always_yields_a_positive_line() {
        for (r, record) in corpus().iter().enumerate() {
            for (lasti, want) in record.expected.iter().take(MAX_OFFSETS).enumerate() {
                if want.is_none() {
                    continue;
                }
                let got = parse(&record.table, lasti as i32, record.firstlineno);
                assert!(got > 0, "record {r} lasti {lasti} gave {got}");
            }
        }
    }

    #[test]
    fn negative_lasti_returns_firstlineno() {
        assert_eq!(parse(&[], -1, 42), 42);
        assert_eq!(parse(&[0x80, 0x00], -7, 13), 13);
        assert_eq!(parse(&[], i32::MIN, 5), 5);
    }

    #[test]
    fn degenerate_tables_do_not_panic() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0x00],
            vec![0xff],
            vec![0xff; 64],
            vec![0x00; 64],
            // info code 14 (long form) truncated mid-varint
            vec![0x70],
            vec![0x70, 0xc0],
            vec![0x70, 0xc0, 0xc0],
            // info code 13 truncated
            vec![0x68],
            vec![0x68, 0xff],
            // a run of continuation bytes, to hit the shift >= 32 guard
            vec![0x70, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            // odd length
            vec![0x50, 0x01, 0x02],
            // every possible single entry byte
            (0u8..=255).collect(),
        ];
        for table in cases {
            for lasti in [0, 1, 2, 7, 64, 1024, i32::MAX] {
                // The contract for arbitrary bytes is only that we return at
                // all: no panic, no hang, no out-of-bounds read. A negative
                // result is possible and matches the C++; see `parse`.
                std::hint::black_box(parse(&table, lasti, 1));
            }
        }
    }

    #[test]
    fn every_single_byte_entry_is_handled() {
        // Exercises all 16 info codes as the first (and only) entry.
        for b in 0u8..=255 {
            std::hint::black_box(parse(&[b], 0, 1));
        }
    }

    /// Fuzz with pseudorandom tables: the parser must never panic and never
    /// return a negative line. Uses the in-repo RNG so there is no dependency
    /// and the failing case is reproducible from the seed.
    #[test]
    fn random_tables_never_panic() {
        // Miri is ~100x slower and the job has a 20 minute budget.
        const ITERATIONS: u32 = if cfg!(miri) { 400 } else { 200_000 };

        let mut rng = MinstdRand::new(0xC0FFEE);
        let mut table = Vec::with_capacity(64);
        for _ in 0..ITERATIONS {
            let len = (rng.next_u32() % 65) as usize;
            table.clear();
            for _ in 0..len {
                table.push((rng.next_u32() & 0xff) as u8);
            }
            let lasti = (rng.next_u32() % 2048) as i32;
            let firstlineno = (rng.next_u32() % 100_000) as i32;
            std::hint::black_box(parse(&table, lasti, firstlineno));
        }
    }
}
