"""Amplified version of the race that kills workers in issue #37.

py-spy samples this process without stopping it, so between the read that gives
it a pointer and the read that dereferences it, the pointed-at object can be
freed and its memory reused. When that object is a string, py-spy's copy_string
decodes whatever bytes it got. For a UCS-4 (kind=4) string it does:

    let chars = unsafe {
        std::slice::from_raw_parts(bytes.as_ptr() as *const char, bytes.len() / 4)
    };
    Ok(chars.iter().collect())

which fabricates Rust `char` values out of arbitrary bytes. `char` is required
to be a Unicode scalar value; anything else is undefined behaviour.

In production that race is won rarely. Here it is won on every single sample:
a fake, immortal PyUnicode header is planted as a code object's co_filename,
and its character data is rewritten with random 32-bit words by another thread
while the sampler decodes it. Nothing here corrupts the interpreter itself --
the fake object is well formed, only its *contents* keep changing, exactly as
they would after a real free/reuse.
"""
import ctypes
import os
import random
import sys
import threading
import time

# CPython 3.12+ object layout (PyCompactUnicodeObject).
OFF_REFCNT, OFF_TYPE, OFF_LENGTH, OFF_HASH, OFF_STATE = 0, 8, 16, 24, 32
OFF_UTF8_LENGTH, OFF_UTF8, OFF_DATA = 40, 48, 56
IMMORTAL = 0xFFFFFFFFFFFFFFFF
# state bitfield: interned:2 | kind:3 | compact:1 | ascii:1 | statically_allocated:1
STATE_UCS4_COMPACT = (4 << 2) | (1 << 5)

NCHARS = int(os.environ.get("NCHARS", "1000"))
DEPTH = int(os.environ.get("DEPTH", "40"))


_libc = ctypes.CDLL(None)
_libc.malloc.restype = ctypes.c_void_p
_libc.malloc.argtypes = [ctypes.c_size_t]


def make_fake_ucs4(nchars):
    """A well-formed, immortal UCS-4 string header whose data we keep rewriting.

    Deliberately allocated with raw malloc and never freed: code objects keep
    pointing at it until interpreter shutdown, and a Python-managed buffer
    would be released first, which would itself be a use-after-free and
    corrupt the heap on its own."""
    addr = _libc.malloc(OFF_DATA + nchars * 4 + 4)
    buf = None
    ctypes.c_uint64.from_address(addr + OFF_REFCNT).value = IMMORTAL
    ctypes.c_uint64.from_address(addr + OFF_TYPE).value = id(str)
    ctypes.c_int64.from_address(addr + OFF_LENGTH).value = nchars
    ctypes.c_int64.from_address(addr + OFF_HASH).value = -1
    ctypes.c_uint32.from_address(addr + OFF_STATE).value = STATE_UCS4_COMPACT
    ctypes.c_int64.from_address(addr + OFF_UTF8_LENGTH).value = 0
    ctypes.c_uint64.from_address(addr + OFF_UTF8).value = 0
    return buf, addr


def find_field(obj_addr, needle, limit=400):
    """Offset of a pointer field inside an object, located by value."""
    words = (ctypes.c_uint64 * (limit // 8)).from_address(obj_addr)
    for i, w in enumerate(words):
        if w == needle:
            return i * 8
    raise RuntimeError("field not found")


def build(depth, filename):
    src = []
    for i in range(depth):
        callee = f"f{i + 1}" if i + 1 < depth else "leaf"
        src.append(f"def f{i}(n):\n    return {callee}(n)\n")
    src.append("def leaf(n):\n    return sum(range(n))\n")
    ns = {}
    exec(compile("\n".join(src), filename, "exec"), ns)
    return [ns[f"f{i}"] for i in range(depth)] + [ns["leaf"]]


def main():
    marker = "\U00010400" * 32  # unique UCS-4 filename, easy to find in the code object
    funcs = build(DEPTH, marker)
    codes = [f.__code__ for f in funcs]
    fake, fake_addr = make_fake_ucs4(NCHARS)

    off = find_field(id(codes[0]), id(codes[0].co_filename))
    for c in codes:
        assert ctypes.c_uint64.from_address(id(c) + off).value == id(c.co_filename)
        ctypes.c_uint64.from_address(id(c) + off).value = fake_addr
    print(f"co_filename -> fake UCS-4 header at 0x{fake_addr:x} "
          f"({NCHARS} chars, offset {off} in PyCodeObject)", flush=True)

    stop = threading.Event()
    data_addr = fake_addr + OFF_DATA
    data_len = NCHARS * 4

    def mutate():
        """Rewrite the character data continuously: every word the sampler reads
        is a fresh random 32-bit value, almost none of them a scalar value."""
        while not stop.is_set():
            src = os.urandom(data_len)
            for _ in range(200):
                ctypes.memmove(data_addr, src, data_len)

    def spin():
        while not stop.is_set():
            funcs[0](50)

    if os.environ.get("NO_PROFILER") == "1":
        # Control: same hostile buffer, same mutation, nobody reading it.
        print("control run: profiler NOT started", flush=True)
    else:
        import pyroscope
        pyroscope.configure(
            application_name="repro37-hostile",
            server_address=os.environ.get("PYROSCOPE_SERVER", "http://127.0.0.1:4040"),
            sample_rate=int(os.environ.get("SAMPLE_RATE", "997")),
            oncpu=False,
            gil_only=False,
            upload_interval=int(os.environ.get("UPLOAD_INTERVAL", "5")),
        )

    threads = [threading.Thread(target=mutate, daemon=True)]
    threads += [threading.Thread(target=spin, daemon=True)
                for _ in range(int(os.environ.get("THREADS", "4")))]
    for t in threads:
        t.start()
    time.sleep(float(os.environ.get("DURATION", "60")))
    stop.set()
    for t in threads:
        t.join(timeout=5)
    print("survived", flush=True)


if __name__ == "__main__":
    main()
