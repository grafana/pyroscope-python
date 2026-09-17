#!/usr/bin/env python3
"""Generate and validate the line-table test corpus for the Rust parser.

CPython 3.11+ stores per-instruction locations in `co_linetable` using the
PEP 657 format. The profiler parses those bytes itself rather than calling
`PyCode_Addr2Line`, because it runs inside the allocator hook where CPython
gives no allocation-free guarantee.

`co_positions()` yields one tuple per code unit, so its index *is* `lasti`.
That makes it an exact oracle for the parser.

  (no args)   regenerate rust/src/memalloc/pure/testdata/linetable_corpus.txt
              from the running interpreter's stdlib
  --verify    (a) check the reference parser below against co_positions() over
              the whole stdlib, and (b) check the committed fixture is
              self-consistent. (b) does not depend on the running version,
              because each record carries its own line-table bytes.

Fixture format, one record per line:

    <firstlineno> <linetable-hex> <rle>

where <rle> is comma-separated `count*line` tokens indexed by lasti, and a
line of `-` marks instructions CPython reports as having no location (the
parser's output there is unconstrained, so the Rust test only requires that
it does not panic).
"""

import argparse
import io
import os
import pathlib
import sys
import types

FIXTURE = pathlib.Path(__file__).resolve().parent.parent / (
    "rust/src/memalloc/pure/testdata/linetable_corpus.txt"
)

# Keep the committed fixture small enough to stay reviewable and to run under
# Miri, which is roughly 100x slower than native.
MAX_FIXTURE_BYTES = 100 * 1024


# --- reference parser: a direct transcription of the C++ being ported -------
# cpp/profiling_helpers/linetable_parser.h, PY_VERSION_HEX >= 0x030b0000 branch.
# Kept in Python so --verify can check it against co_positions() independently
# of the Rust port.

def _read_varint(table, n, i):
    guard = n - 1
    if i[0] >= guard:
        return 0
    i[0] += 1
    val = table[i[0]] & 63
    shift = 0
    while i[0] < guard and (table[i[0]] & 64):
        shift += 6
        if shift >= 32:
            while i[0] < guard and (table[i[0]] & 64):
                i[0] += 1
            return 0
        i[0] += 1
        val |= (table[i[0]] & 63) << shift
    return val


def _read_signed_varint(table, n, i):
    v = _read_varint(table, n, i)
    return -(v >> 1) if (v & 1) else (v >> 1)


def parse_linetable(table, lasti, firstlineno):
    n = len(table)
    if lasti < 0:
        return firstlineno
    lineno = firstlineno
    i = [0]
    bc = 0
    while i[0] < n:
        b = table[i[0]]
        bc += (b & 7) + 1
        info = (b >> 3) & 15
        if info == 15:
            pass
        elif info == 14:
            lineno += _read_signed_varint(table, n, i)
            _read_varint(table, n, i)
            _read_varint(table, n, i)
            _read_varint(table, n, i)
        elif info == 13:
            lineno += _read_signed_varint(table, n, i)
        elif info in (10, 11, 12):
            lineno += info - 10
            if i[0] < n - 2:
                i[0] += 2
        else:
            if i[0] < n - 1:
                i[0] += 1
        if bc > lasti:
            break
        i[0] += 1
    return lineno if lineno > 0 else 0


# --- corpus collection ------------------------------------------------------

def walk_codes(code, seen):
    if id(code) in seen:
        return
    seen.add(id(code))
    yield code
    for const in code.co_consts:
        if isinstance(const, types.CodeType):
            yield from walk_codes(const, seen)


def stdlib_codes():
    """Compile every stdlib .py we can and collect all code objects.

    Compiling rather than importing keeps this side-effect free and gets us
    far more code objects than walking module globals.
    """
    stdlib = pathlib.Path(os.path.dirname(os.__file__))
    seen = set()
    codes = []
    for path in sorted(stdlib.rglob("*.py")):
        if "test" in path.parts or "idlelib" in path.parts:
            continue
        try:
            src = path.read_text(encoding="utf-8", errors="strict")
            top = compile(src, str(path), "exec", dont_inherit=True)
        except (SyntaxError, UnicodeDecodeError, ValueError, RecursionError, OSError):
            continue
        codes.extend(walk_codes(top, seen))
    return codes


def expectations(code):
    """[(lasti, expected_line_or_None)] straight from CPython."""
    return [(i, pos[0]) for i, pos in enumerate(code.co_positions())]


def info_codes(table):
    """Which PEP 657 info codes a table actually exercises."""
    out, i, n = set(), 0, len(table)
    while i < n:
        info = (table[i] >> 3) & 15
        out.add(info)
        if info == 14:
            ii = [i]
            _read_signed_varint(table, n, ii)
            _read_varint(table, n, ii)
            _read_varint(table, n, ii)
            _read_varint(table, n, ii)
            i = ii[0]
        elif info == 13:
            ii = [i]
            _read_signed_varint(table, n, ii)
            i = ii[0]
        elif info in (10, 11, 12):
            if i < n - 2:
                i += 2
        elif info != 15:
            if i < n - 1:
                i += 1
        i += 1
    return out


def has_negative_delta(table):
    i, n = 0, len(table)
    while i < n:
        info = (table[i] >> 3) & 15
        if info in (13, 14):
            ii = [i]
            if _read_signed_varint(table, n, ii) < 0:
                return True
            if info == 14:
                _read_varint(table, n, ii)
                _read_varint(table, n, ii)
                _read_varint(table, n, ii)
            i = ii[0]
        elif info in (10, 11, 12):
            if i < n - 2:
                i += 2
        elif info != 15:
            if i < n - 1:
                i += 1
        i += 1
    return False


def rle(values):
    out = []
    for v in values:
        token = "-" if v is None else str(v)
        if out and out[-1][0] == token:
            out[-1][1] += 1
        else:
            out.append([token, 1])
    return ",".join(f"{n}*{t}" for t, n in out)


def record(code):
    exp = expectations(code)
    return "{} {} {}".format(
        code.co_firstlineno,
        code.co_linetable.hex(),
        rle([line for _, line in exp]),
    )


def select(codes):
    """Pick a diverse subset that fits the size budget.

    Prioritise tables exercising the rarer info codes (14 = long form,
    13 = no column data, 15 = no location) and negative line deltas, since
    those are the branches most likely to be mis-ported.
    """
    def priority(code):
        table = code.co_linetable
        codes_used = info_codes(table)
        score = 0
        if 14 in codes_used:
            score -= 8
        if 13 in codes_used:
            score -= 4
        if 15 in codes_used:
            score -= 2
        if has_negative_delta(table):
            score -= 8
        return (score, -len(table))

    ranked = sorted({c.co_linetable: c for c in codes}.values(), key=priority)

    picked, total = [], 0
    for code in ranked:
        line = record(code)
        if total + len(line) + 1 > MAX_FIXTURE_BYTES:
            continue
        picked.append(line)
        total += len(line) + 1
        if total > MAX_FIXTURE_BYTES * 0.98:
            break
    return picked


def generate():
    codes = stdlib_codes()
    if not codes:
        sys.exit("collected no code objects")
    picked = select(codes)
    header = [
        "# PEP 657 line-table corpus. Generated by scripts/gen_linetable_corpus.py",
        "# from CPython {}. Do not edit by hand.".format(sys.version.split()[0]),
        "# Format: <firstlineno> <linetable-hex> <rle of expected line per lasti>",
        "# A line of '-' means CPython reports no location for that instruction.",
    ]
    FIXTURE.parent.mkdir(parents=True, exist_ok=True)
    body = "\n".join(header + picked) + "\n"
    io.open(FIXTURE, "w", encoding="utf-8", newline="\n").write(body)
    offsets = sum(len(l.split(" ")[2].split(",")) for l in picked)
    print("wrote {} ({} records, {} RLE runs, {} bytes)".format(
        FIXTURE, len(picked), offsets, len(body)))


def parse_fixture():
    records = []
    for raw in io.open(FIXTURE, encoding="utf-8"):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        firstlineno, hexed, runs = line.split(" ")
        expected = []
        for token in runs.split(","):
            n, _, val = token.partition("*")
            expected.extend([None if val == "-" else int(val)] * int(n))
        records.append((int(firstlineno), bytes.fromhex(hexed), expected))
    return records


def verify():
    rc = 0

    # (a) the reference parser vs CPython itself, over the whole stdlib
    total = mismatched = 0
    for code in stdlib_codes():
        table = code.co_linetable
        for lasti, want in expectations(code):
            if want is None:
                continue
            total += 1
            if parse_linetable(table, lasti, code.co_firstlineno) != want:
                mismatched += 1
    print("reference parser vs co_positions() on CPython {}: "
          "{} offsets, {} mismatches".format(
              sys.version.split()[0], total, mismatched))
    if mismatched or total == 0:
        rc = 1

    # (b) the committed fixture is self-consistent. Version independent: each
    # record carries its own line-table bytes.
    if not FIXTURE.exists():
        print("fixture missing: {}".format(FIXTURE))
        return 1
    fixture_total = fixture_bad = 0
    for firstlineno, table, expected in parse_fixture():
        for lasti, want in enumerate(expected):
            if want is None:
                continue
            fixture_total += 1
            if parse_linetable(table, lasti, firstlineno) != want:
                fixture_bad += 1
    print("committed fixture self-check: {} offsets, {} mismatches".format(
        fixture_total, fixture_bad))
    if fixture_bad or fixture_total == 0:
        rc = 1
    return rc


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--verify", action="store_true",
                    help="validate instead of regenerating")
    args = ap.parse_args()
    sys.exit(verify() if args.verify else (generate() or 0))


if __name__ == "__main__":
    main()
