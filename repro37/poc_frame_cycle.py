"""py-spy walks the profiled interpreter's frame chain with no bound:

    while !frame_ptr.is_null() {
        let frame = process.copy_pointer(frame_ptr)...;   // stack_trace.rs
        ...
        frames.push(Frame { name, filename, ... });
        frame_ptr = frame.back();
    }

Because the walk is done with unsynchronized reads of a *live* interpreter, a
torn or stale `previous` pointer can point back at a frame the walk has already
visited -- the interpreter's data-stack chunks are constantly reused, so a
pointer that was valid a moment ago can now address a re-pushed frame. The loop
then never terminates and `frames` grows until the process dies.

This script produces that situation deterministically: a thread parks forever
inside a Python frame, and that frame's `previous` pointer is made to point at
itself. Nothing else about the interpreter is changed, and CPython itself never
follows the pointer (the parked frame is never returned from).

Run:  python poc_frame_cycle.py        (NO_PATCH=1 for the control run)
"""
import ctypes
import os
import resource
import sys
import threading
import time

# PyFrameObject: PyObject_HEAD, PyFrameObject *f_back, _PyInterpreterFrame *f_frame
OFF_F_BACK = 16
OFF_F_FRAME = 24


def word(addr):
    return ctypes.c_uint64.from_address(addr).value


def set_word(addr, value):
    ctypes.c_uint64.from_address(addr).value = value


def iframe_of(frame_obj):
    """The _PyInterpreterFrame behind a PyFrameObject.

    Validated rather than assumed: the candidate must hold a pointer to this
    frame's code object in its first few words (f_executable on 3.12+, f_code
    on 3.11)."""
    frame_obj.f_back            # materialize, it is filled in lazily on 3.11+
    candidate = word(id(frame_obj) + OFF_F_FRAME)
    code_addr = id(frame_obj.f_code)
    for off in range(0, 64, 8):
        if word(candidate + off) == code_addr:
            return candidate
    raise RuntimeError("could not validate _PyInterpreterFrame")


def find_previous_offset(inner_iframe, outer_iframe, limit=200):
    """Offset of `previous` inside _PyInterpreterFrame (it moved between
    versions), located by value: it is the only field holding the address of
    the calling frame."""
    for off in range(0, limit, 8):
        if word(inner_iframe + off) == outer_iframe:
            return off
    raise RuntimeError("previous field not found")


parked = threading.Event()
ready = threading.Event()


def level3():
    ready.set()
    parked.wait()          # parks here forever; this frame is never returned from


def level2():
    level3()


def level1():
    level2()


def main():
    victim = threading.Thread(target=level1, daemon=True)
    victim.start()
    ready.wait()
    time.sleep(0.3)

    frames = sys._current_frames()
    inner = frames[victim.ident]
    outer = inner.f_back
    inner_if = iframe_of(inner)
    outer_if = iframe_of(outer)
    off = find_previous_offset(inner_if, outer_if)
    print(f"parked frame {inner.f_code.co_name} iframe=0x{inner_if:x} "
          f"previous at +{off}", flush=True)

    if os.environ.get("NO_PATCH") == "1":
        print("control run: frame chain left intact", flush=True)
    else:
        set_word(inner_if + off, inner_if)      # previous -> itself
        print("patched: parked frame's previous now points at itself", flush=True)

    import pyroscope
    pyroscope.configure(
        application_name="repro37-frame-cycle",
        server_address=os.environ.get("PYROSCOPE_SERVER", "http://127.0.0.1:4040"),
        sample_rate=int(os.environ.get("SAMPLE_RATE", "100")),
        oncpu=False,
        gil_only=False,
        upload_interval=int(os.environ.get("UPLOAD_INTERVAL", "10")),
    )

    deadline = time.time() + float(os.environ.get("DURATION", "60"))
    while time.time() < deadline:
        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss // 1024
        print(f"t={time.time() - (deadline - 60):5.1f}s rss={rss} MiB", flush=True)
        time.sleep(1)
    print("survived", flush=True)
    os._exit(0)                                  # parked thread cannot be joined


if __name__ == "__main__":
    main()
