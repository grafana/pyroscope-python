"""Deterministic proof of the unsound path py-spy takes when it reads a
UCS-4 (kind=4) string out of the profiled process.

py-spy's copy_string does:

    let chars = unsafe {
        std::slice::from_raw_parts(bytes.as_ptr() as *const char, bytes.len() / 4)
    };
    Ok(chars.iter().collect())

i.e. it reinterprets whatever 4-byte words it read as Rust `char`s. `char` is
required to be a Unicode scalar value (<= 0x10FFFF, no surrogates); producing
one that isn't is UB. In production this happens by accident: the sampler reads
a code object / string that was freed and reused between two of its reads.

This script makes it happen on purpose: it builds a code object whose
co_filename is a UCS-4 string, then rewrites that string's raw data with word
values that are not valid scalar values, and executes it in a deep stack so the
sampler decodes it thousands of times a second.

Run: .venv/bin/python poc_ucs4_garbage.py
"""
import ctypes
import os
import sys
import threading
import time

# CPython 3.12+: the data of a compact, non-ASCII string starts right after
# PyCompactUnicodeObject.
DATA_OFFSET = 56
NCHARS = int(os.environ.get("NCHARS", "500"))

# Not Unicode scalar values: above 0x10FFFF, and lone surrogates.
GARBAGE = [0xFFFFFFFF, 0x80000000, 0x7FFFFFFF, 0x0000D800, 0x0010FFFF + 1, 0xDEADBEEF]


def make_ucs4(n):
    """A compact UCS-4 string (kind=4) of n characters."""
    s = "\U00010400" * n
    assert sys.getsizeof(s) == DATA_OFFSET + n * 4 + 4, "unexpected string layout"
    return s


def poison(s, values):
    buf = (ctypes.c_uint32 * len(s)).from_address(id(s) + DATA_OFFSET)
    for i in range(len(s)):
        buf[i] = values[i % len(values)]


def build(depth, filename):
    src = []
    for i in range(depth):
        callee = f"f{i + 1}" if i + 1 < depth else "leaf"
        src.append(f"def f{i}(n):\n    return {callee}(n)\n")
    src.append("def leaf(n):\n    return sum(range(n))\n")
    ns = {}
    exec(compile("\n".join(src), filename, "exec"), ns)
    return ns["f0"]


def main():
    filename = make_ucs4(NCHARS)
    fn = build(int(os.environ.get("DEPTH", "40")), filename)
    assert fn.__code__.co_filename is filename
    poison(filename, GARBAGE)
    print(f"poisoned co_filename ({NCHARS} chars of non-scalar values)", flush=True)

    import pyroscope
    pyroscope.configure(
        application_name="repro37-poc",
        server_address=os.environ.get("PYROSCOPE_SERVER", "http://127.0.0.1:4040"),
        sample_rate=int(os.environ.get("SAMPLE_RATE", "997")),
        oncpu=False,
        gil_only=False,
        upload_interval=int(os.environ.get("UPLOAD_INTERVAL", "5")),
    )

    stop = threading.Event()

    def spin():
        while not stop.is_set():
            fn(50)

    threads = [threading.Thread(target=spin, daemon=True)
               for _ in range(int(os.environ.get("THREADS", "4")))]
    for t in threads:
        t.start()
    time.sleep(float(os.environ.get("DURATION", "30")))
    stop.set()
    for t in threads:
        t.join(timeout=5)
    print("survived", flush=True)


if __name__ == "__main__":
    main()
