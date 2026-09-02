"""Deterministic reproducer for grafana/pyroscope-python#37 (Python 3.11).

Root cause
----------
`remoteprocess::ProcessMemory::copy_struct` materializes a `T` out of bytes
read from the profiled process:

    fn copy_struct<T: Copy>(&self, addr: usize) -> Result<T, Error> {
        let mut data = vec![0; std::mem::size_of::<T>()];
        self.read(addr, &mut data)?;
        Ok(unsafe { std::ptr::read(data.as_ptr() as *const _) })
    }

`T: Copy` does not mean "every bit pattern is a valid T". For Python 3.11,
py-spy's `_PyInterpreterFrame` binding contains

    pub is_entry: bool,      // offset 68

and a Rust `bool` may only hold 0 or 1, so it is a *niche*. rustc uses that
niche as the discriminant of the returned `Result`:

    sizeof(Result<_PyInterpreterFrame, remoteprocess::Error>) == 80
    sizeof(_PyInterpreterFrame)                               == 80

i.e. there is no room for a separate tag. `Err` is encoded as "the byte at
offset 68 is out of range for bool".

So when the sampler reads a frame whose byte 68 is >= 2 -- which is what a
stale or torn read of the live interpreter returns -- `copy_pointer` silently
returns `Err(remoteprocess::Error)` whose payload is the *first 32 bytes of the
memory that was read*. py-spy wraps it in a context and ships it in
`Sample.sampling_errors`; when pyroscope's consumer thread drops that Sample,
`drop_glue<remoteprocess::Error>` runs over those bytes and calls `free()` on
whatever they happen to contain:

    #7  free ()
    #8  anyhow::error::object_drop ()
    #9  anyhow::error::object_drop ()
    #10 std::sys::backtrace::__rust_begin_short_backtrace ()

That is the stack in the issue, with `free(mem=0x18)` -- a field of a CPython
object read as a String pointer.

What this script does
---------------------
Instead of waiting to lose the race, it plants the situation directly: a
thread parks inside a Python frame, and that frame's `previous` pointer is
made to point at a fake frame whose byte 68 is 2 and whose first bytes decode
as `remoteprocess::Error::…(String)` with a bogus pointer. Every sample then
hits the bad path, so the process dies in seconds instead of hours.

Nothing here corrupts the interpreter: the fake frame is a private malloc'd
buffer, and CPython never reads it (the parked frame is never returned from).

Run (Python 3.11):  python poc_niche_confusion.py
"""
import ctypes
import os
import sys
import threading
import time

# CPython 3.11 layouts.
FRAMEOBJ_F_FRAME = 24          # PyFrameObject.f_frame
IFRAME_PREVIOUS = 48           # _PyInterpreterFrame.previous
IFRAME_IS_ENTRY = 68           # _PyInterpreterFrame.is_entry (Rust `bool`)
IFRAME_SIZE = 80

BOGUS_PTR = 0x29               # what free() will be called with
FAKE_SIZE = 4096

libc = ctypes.CDLL(None)
libc.malloc.restype = ctypes.c_void_p
libc.malloc.argtypes = [ctypes.c_size_t]


def w64(addr, value):
    ctypes.c_uint64.from_address(addr).value = value


def r64(addr):
    return ctypes.c_uint64.from_address(addr).value


def iframe_of(frame_obj):
    frame_obj.f_back                      # materialized lazily on 3.11+
    return r64(id(frame_obj) + FRAMEOBJ_F_FRAME)


def make_fake_frame():
    """A frame-shaped buffer that is a valid `Err` and decodes to a String
    whose pointer is bogus."""
    addr = libc.malloc(FAKE_SIZE)
    ctypes.memset(addr, 0, FAKE_SIZE)
    # remoteprocess::Error is 32 bytes and shares these bytes with the frame.
    # Its discriminant sits in the first word; small values select the
    # variants that own a String. Both the Other(String) and
    # GoblinError(Malformed(String)) shapes read a length and a pointer from
    # the words that follow, so fill every candidate slot.
    for off in (0x00, 0x20):
        w64(addr + off, 3)                # discriminant-ish: small
    for off in (0x08, 0x18, 0x28):
        w64(addr + off, 0x100)            # capacity / length
    for off in (0x10, 0x20):
        w64(addr + off, BOGUS_PTR)        # String pointer -> free(0x29)
    w64(addr + IFRAME_PREVIOUS, 0)        # terminate the frame walk
    ctypes.c_uint8.from_address(addr + IFRAME_IS_ENTRY).value = 2   # not a bool
    return addr


parked = threading.Event()
ready = threading.Event()


def level2():
    ready.set()
    parked.wait()


def level1():
    level2()


def main():
    if sys.version_info[:2] != (3, 11):
        print(f"this probe encodes 3.11 offsets; running on {sys.version_info[:2]}",
              file=sys.stderr)

    victim = threading.Thread(target=level1, daemon=True)
    victim.start()
    ready.wait()
    time.sleep(0.3)

    frame = sys._current_frames()[victim.ident]
    iframe = iframe_of(frame)
    outer = iframe_of(frame.f_back)
    assert r64(iframe + IFRAME_PREVIOUS) == outer, "unexpected _PyInterpreterFrame layout"

    fake = make_fake_frame()
    w64(iframe + IFRAME_PREVIOUS, fake)
    print(f"parked frame {frame.f_code.co_name}: previous -> fake frame 0x{fake:x} "
          f"(is_entry=2, String ptr=0x{BOGUS_PTR:x})", flush=True)

    if os.environ.get("NO_PROFILER") == "1":
        print("control run: profiler NOT started", flush=True)
    else:
        import pyroscope
        pyroscope.configure(
            application_name="repro37-niche",
            server_address=os.environ.get("PYROSCOPE_SERVER", "http://127.0.0.1:4040"),
            sample_rate=int(os.environ.get("SAMPLE_RATE", "100")),
            oncpu=False,
            gil_only=False,
            upload_interval=int(os.environ.get("UPLOAD_INTERVAL", "10")),
        )

    deadline = time.time() + float(os.environ.get("DURATION", "30"))
    while time.time() < deadline:
        time.sleep(0.5)
    print("survived", flush=True)
    os._exit(0)


if __name__ == "__main__":
    main()
