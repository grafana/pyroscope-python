#!/usr/bin/env bash
# Check how the memory profiler's reentrancy guard thread-local was compiled.
#
# The guard is read from inside CPython's allocator, so two properties matter,
# and neither is expressible in stable Rust:
#
#   1. No lazy initialisation and no thread-local destructor. A
#      `thread_local!` with a `const` initialiser over a type that needs no
#      Drop compiles to a bare `#[thread_local]` static; std picks that branch
#      on `!mem::needs_drop::<T>()`. The other branch wraps the value in
#      LazyStorage or EagerStorage, which means an init check on every access
#      and a destructor registered per thread -- and the first access would
#      then allocate, inside an allocator hook.
#
#      Checked by symbol name, so it is attributable to our thread-local
#      rather than to the two dozen others pyo3 and py-spy bring in. The
#      runtime proof is `acquiring_the_guard_does_not_allocate` in
#      rust/src/memalloc/reentrancy.rs, which counts allocator traffic.
#
#   2. A dynamic TLS model. The extension is dlopen'ed, and an initial-exec
#      model makes the import fail with "cannot allocate memory in static TLS
#      block" once enough other TLS-heavy extensions load first. A cdylib is
#      compiled PIC and LLVM defaults to a dynamic model there, but nothing in
#      the source says so, so a toolchain change could regress it silently.
#      The C++ asked for this explicitly via tls_model("global-dynamic").
#
# Usage: scripts/check_tls_model.sh <path to the built .so or .dylib>

set -euo pipefail

if [ $# -ne 1 ]; then
    echo "usage: $0 <extension .so|.dylib>" >&2
    exit 2
fi

lib="$1"
if [ ! -f "$lib" ]; then
    echo "not a file: $lib" >&2
    exit 2
fi

echo "checking $lib"
failures=0

symbols="$(nm "$lib" 2>/dev/null || true)"
if [ -z "$symbols" ]; then
    echo "  could not read symbols" >&2
    exit 2
fi

guard_symbols="$(echo "$symbols" | grep "memalloc.*reentrancy.*IN_HOOK" || true)"

if [ -z "$guard_symbols" ]; then
    echo "  FAIL: no IN_HOOK thread-local symbol found; did the guard move or get renamed?" >&2
    failures=$((failures + 1))
else
    # std's const-init, !needs_drop branch emits __RUST_STD_INTERNAL_VAL
    # directly. The lazy branches name LazyStorage or EagerStorage instead.
    if echo "$guard_symbols" | grep -q "RUST_STD_INTERNAL_VAL"; then
        echo "  ok: the guard is a bare #[thread_local] static (const-init, no Drop)"
    else
        echo "  FAIL: IN_HOOK is not a bare thread-local:" >&2
        echo "$guard_symbols" | sed 's/^/    /' >&2
        failures=$((failures + 1))
    fi
    if echo "$guard_symbols" | grep -qE "LazyStorage|EagerStorage"; then
        echo "  FAIL: the guard went through lazy storage, so it initialises on first access" >&2
        failures=$((failures + 1))
    else
        echo "  ok: no lazy or eager storage wrapper (no init check, no destructor)"
    fi
fi

case "$(uname -s)" in
Darwin)
    if otool -l "$lib" | grep -q "__thread_vars"; then
        echo "  ok: __DATA,__thread_vars present"
    else
        echo "  FAIL: no __DATA,__thread_vars section" >&2
        failures=$((failures + 1))
    fi
    # Mach-O reaches thread-locals through _tlv_get_addr and has no
    # static-TLS model to reject, so there is nothing further to check here.
    ;;
Linux)
    relocs="$(readelf -r "$lib" 2>/dev/null || true)"
    if echo "$relocs" | grep -qE "TLSGD|TLSLD|TLSDESC|DTPMOD|DTPOFF"; then
        echo "  ok: dynamic TLS relocations present"
    else
        echo "  FAIL: no dynamic TLS relocations found" >&2
        failures=$((failures + 1))
    fi
    # These are the models that break under dlopen. Nothing we compile should
    # use them, so rejecting them anywhere in our own object is correct.
    if echo "$relocs" | grep -qE "GOTTPOFF|TPOFF32|TPOFF64|_TLSIE_|_TLSLE_"; then
        echo "  FAIL: initial-exec or local-exec TLS relocations found" >&2
        echo "$relocs" | grep -E "GOTTPOFF|TPOFF32|TPOFF64|_TLSIE_|_TLSLE_" | head -5 | sed 's/^/    /' >&2
        failures=$((failures + 1))
    else
        echo "  ok: no static-TLS (initial-exec/local-exec) relocations"
    fi
    ;;
*)
    echo "  skipped: unsupported platform $(uname -s)"
    exit 0
    ;;
esac

if [ "$failures" -ne 0 ]; then
    echo "$failures TLS check(s) failed" >&2
    exit 1
fi
echo "all TLS checks passed"
