#!/usr/bin/env python3
"""Cross-check the profiler's CPython offsets against ctypes.

The Rust memory profiler locates CPython's internal frame and code-object
fields through `_Py_DebugOffsets`, a self-describing table of `offsetof()`
values that CPython 3.13+ places as the first member of `_PyRuntimeState`. The
Rust side has a hand-transcribed `#[repr(C)]` mirror of that table per minor
version, and a transcription error there would shift every field after it.

This reads the same table independently, through ctypes, and asserts that every
value the extension reports matches. Run it on each supported CPython and each
platform when bringing up a new version:

    python scripts/check_debug_offsets.py

Exits non-zero on any mismatch. On an interpreter without memory profiling
support (below 3.13, or free-threaded) it reports that and exits 0, since
declining cleanly is the intended behaviour there.
"""

import ctypes
import struct
import sys

COOKIE = b"xdebugpy"

# Layout of _Py_DebugOffsets, per minor version. Every field is a uint64_t
# except the leading 8-byte cookie. Mirrors rust/src/memalloc/pure/offsets.rs;
# the point of the exercise is that these two were transcribed independently.
LAYOUTS = {
    (3, 13): [
        ("header", ["cookie", "version", "free_threaded"]),
        ("runtime_state", ["size", "finalizing", "interpreters_head"]),
        ("interpreter_state", [
            "size", "id", "next", "threads_head", "gc", "imports_modules",
            "sysdict", "builtins", "ceval_gil", "gil_runtime_state",
            "gil_runtime_state_enabled", "gil_runtime_state_locked",
            "gil_runtime_state_holder"]),
        ("thread_state", [
            "size", "prev", "next", "interp", "current_frame", "thread_id",
            "native_thread_id", "datastack_chunk", "status"]),
        ("interpreter_frame", [
            "size", "previous", "executable", "instr_ptr", "localsplus",
            "owner"]),
        ("code_object", [
            "size", "filename", "name", "qualname", "linetable", "firstlineno",
            "argcount", "localsplusnames", "localspluskinds",
            "co_code_adaptive"]),
        ("pyobject", ["size", "ob_type"]),
        ("type_object", ["size", "tp_name", "tp_repr", "tp_flags"]),
        ("tuple_object", ["size", "ob_item", "ob_size"]),
        ("list_object", ["size", "ob_item", "ob_size"]),
        ("dict_object", ["size", "ma_keys", "ma_values"]),
        ("float_object", ["size", "ob_fval"]),
        ("long_object", ["size", "lv_tag", "ob_digit"]),
        ("bytes_object", ["size", "ob_size", "ob_sval"]),
        ("unicode_object", ["size", "state", "length", "asciiobject_size"]),
        ("gc", ["size", "collecting"]),
    ],
    (3, 14): [
        ("header", ["cookie", "version", "free_threaded"]),
        ("runtime_state", ["size", "finalizing", "interpreters_head"]),
        ("interpreter_state", [
            "size", "id", "next", "threads_head", "threads_main", "gc",
            "imports_modules", "sysdict", "builtins", "ceval_gil",
            "gil_runtime_state", "gil_runtime_state_enabled",
            "gil_runtime_state_locked", "gil_runtime_state_holder",
            "code_object_generation", "tlbc_generation"]),
        ("thread_state", [
            "size", "prev", "next", "interp", "current_frame", "thread_id",
            "native_thread_id", "datastack_chunk", "status"]),
        ("interpreter_frame", [
            "size", "previous", "executable", "instr_ptr", "localsplus",
            "owner", "stackpointer", "tlbc_index"]),
        ("code_object", [
            "size", "filename", "name", "qualname", "linetable", "firstlineno",
            "argcount", "localsplusnames", "localspluskinds",
            "co_code_adaptive", "co_tlbc"]),
        ("pyobject", ["size", "ob_type"]),
        ("type_object", ["size", "tp_name", "tp_repr", "tp_flags"]),
        ("tuple_object", ["size", "ob_item", "ob_size"]),
        ("list_object", ["size", "ob_item", "ob_size"]),
        ("set_object", ["size", "used", "table", "mask"]),
        ("dict_object", ["size", "ma_keys", "ma_values"]),
        ("float_object", ["size", "ob_fval"]),
        ("long_object", ["size", "lv_tag", "ob_digit"]),
        ("bytes_object", ["size", "ob_size", "ob_sval"]),
        ("unicode_object", ["size", "state", "length", "asciiobject_size"]),
        ("gc", ["size", "collecting"]),
        ("gen_object", ["size", "gi_name", "gi_iframe", "gi_frame_state"]),
        ("llist_node", ["next", "prev"]),
        ("debugger_support", [
            "eval_breaker", "remote_debugger_support",
            "remote_debugging_enabled", "debugger_pending_call",
            "debugger_script_path", "debugger_script_path_size"]),
    ],
}

# Field names the extension reports, mapped to (group, field) in the table.
EXPECTED = {
    "thread_state_size": ("thread_state", "size"),
    "thread_state_current_frame": ("thread_state", "current_frame"),
    "frame_size": ("interpreter_frame", "size"),
    "frame_previous": ("interpreter_frame", "previous"),
    "frame_executable": ("interpreter_frame", "executable"),
    "frame_instr_ptr": ("interpreter_frame", "instr_ptr"),
    "frame_owner": ("interpreter_frame", "owner"),
    "code_size": ("code_object", "size"),
    "code_filename": ("code_object", "filename"),
    "code_name": ("code_object", "name"),
    "code_qualname": ("code_object", "qualname"),
    "code_linetable": ("code_object", "linetable"),
    "code_firstlineno": ("code_object", "firstlineno"),
    "code_co_code_adaptive": ("code_object", "co_code_adaptive"),
    "bytes_ob_size": ("bytes_object", "ob_size"),
    "bytes_ob_sval": ("bytes_object", "ob_sval"),
    "unicode_size": ("unicode_object", "size"),
    "unicode_state": ("unicode_object", "state"),
    "unicode_length": ("unicode_object", "length"),
    "unicode_asciiobject_size": ("unicode_object", "asciiobject_size"),
    "pyobject_ob_type": ("pyobject", "ob_type"),
    "type_tp_flags": ("type_object", "tp_flags"),
}


def read_table_via_ctypes():
    """Read _Py_DebugOffsets independently of the extension."""
    version = (sys.version_info.major, sys.version_info.minor)
    layout = LAYOUTS.get(version)
    if layout is None:
        return None, f"no ctypes layout transcribed for CPython {version[0]}.{version[1]}"

    try:
        handle = ctypes.CDLL(None)._PyRuntime
    except AttributeError:
        return None, "this interpreter does not export _PyRuntime"
    base = ctypes.cast(handle, ctypes.c_void_p).value

    index = {}
    slot = 0
    for group, fields in layout:
        for field in fields:
            index[(group, field)] = slot
            slot += 1

    raw = (ctypes.c_uint64 * slot).from_address(base)
    cookie = struct.pack("<Q", raw[index[("header", "cookie")]])
    if cookie != COOKIE:
        return None, f"bad cookie {cookie!r}; _Py_DebugOffsets arrived in 3.13"

    hexver = raw[index[("header", "version")]]
    got = ((hexver >> 24) & 0xFF, (hexver >> 16) & 0xFF)
    if got != version:
        return None, f"table reports {got[0]}.{got[1]}, interpreter is {version[0]}.{version[1]}"

    return {key: raw[slot] for key, slot in index.items()}, None


def main():
    try:
        from pyroscope import _native
    except ImportError as exc:
        sys.exit(f"could not import pyroscope._native: {exc}")

    if not hasattr(_native, "_debug_offsets_selftest"):
        sys.exit(
            "this build has no _debug_offsets_selftest. Rebuild with the\n"
            "diagnostics compiled in:\n\n"
            "    PYROSCOPE_DEBUG_INTROSPECTION=1 pip install --force-reinstall .\n"
        )

    report = _native._debug_offsets_selftest()
    print(f"interpreter      : CPython {sys.version.split()[0]}")
    print("extension built for: CPython {}.{}".format(*report["build_version"]))
    print(f"supported        : {report['supported']}")
    if report["error"]:
        print(f"error            : {report['error']}")

    table, why = read_table_via_ctypes()

    if not report["supported"]:
        # Declining is the intended behaviour below 3.13 and on free-threaded
        # builds. Make sure ctypes agrees there is nothing usable here, so a
        # real regression cannot hide behind an expected refusal.
        if table is not None:
            sys.exit(
                "MISMATCH: the extension declined, but ctypes found a valid "
                "offsets table for this interpreter"
            )
        print(f"ctypes agrees    : {why}")
        print("\nOK (memory profiling correctly unavailable here)")
        return

    if table is None:
        sys.exit(f"MISMATCH: the extension resolved offsets but ctypes could not: {why}")

    mismatches = []
    for name, key in sorted(EXPECTED.items()):
        want = table[key]
        got = report["offsets"].get(name)
        if got is None:
            mismatches.append(f"{name}: not reported by the extension")
        elif got != want:
            mismatches.append(f"{name}: extension says {got}, ctypes says {want}")

    extra = set(report["offsets"]) - set(EXPECTED)
    for name in sorted(extra):
        mismatches.append(f"{name}: reported by the extension but not checked here")

    width = max(len(n) for n in EXPECTED)
    print()
    for name, key in sorted(EXPECTED.items()):
        print(f"  {name:<{width}} {report['offsets'].get(name)}")

    if mismatches:
        print("\n".join(["", "MISMATCHES:"] + mismatches), file=sys.stderr)
        sys.exit(1)
    print(f"\nOK ({len(EXPECTED)} offsets agree with an independent ctypes read)")


if __name__ == "__main__":
    main()
