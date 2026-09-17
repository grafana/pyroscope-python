#!/usr/bin/env python3
"""Compare the profiler's frame walker against CPython's own traceback module.

The memory profiler cannot use `PyThreadState_GetFrame` / `PyFrame_GetBack` /
`PyFrame_GetCode` to walk the stack: each returns a new reference, and a decref
inside an allocator hook can free, which re-enters the hook. So it walks the
frame chain by reading struct fields directly, through the offsets CPython
publishes in `_Py_DebugOffsets`.

This exercises that walker from Python and asserts it agrees with
`traceback.extract_stack()` across the shapes that actually differ in CPython's
frame representation: generators, comprehensions, async frames, class bodies,
frames below a C call, and deep recursion.

Run on each supported CPython and platform when bringing up a new version:

    python scripts/check_frame_walk.py

Exits non-zero on disagreement.
"""

import asyncio
import sys
import traceback

failures = []
checks = 0


def reference_stack():
    """CPython's own view, innermost first, excluding this helper."""
    return [
        (frame.name, frame.filename, frame.lineno)
        for frame in reversed(traceback.extract_stack()[:-1])
    ]


def compare(label, walked, reference):
    """Compare the two stacks over the frames they have in common.

    Only line numbers of the *outer* frames are compared. The innermost frame
    legitimately differs: the two collectors are called from adjacent
    statements, so `lasti` is not the same for the frame they were called from.
    """
    global checks
    checks += 1

    # The walker reports co_qualname (Class.method), traceback reports
    # co_name (method). Compare on the last dotted component.
    def norm(name):
        return name.rsplit(".", 1)[-1]

    walked_names = [norm(n) for n, _, _ in walked]
    ref_names = [norm(n) for n, _, _ in reference]

    if walked_names != ref_names:
        failures.append(
            f"{label}: name mismatch\n"
            f"  walker   : {walked_names}\n"
            f"  traceback: {ref_names}"
        )
        return

    for i, ((wn, wf, wl), (rn, rf, rl)) in enumerate(zip(walked, reference)):
        if wf != rf:
            failures.append(f"{label}: frame {i} ({wn}) file {wf!r} != {rf!r}")
        # Skip the innermost frame's line, for the reason above.
        if i > 0 and wl != rl:
            failures.append(f"{label}: frame {i} ({wn}) line {wl} != {rl}")


def check(label, walk):
    """Capture both stacks at the same point and compare."""
    compare(label, walk(), reference_stack())


# --- the frame shapes worth exercising ------------------------------------

def at_module_level(walk):
    check("module-level call", walk)


def in_a_plain_function(walk):
    check("plain function", walk)


def in_a_nested_function(walk):
    def inner():
        check("nested function", walk)
    inner()


class Holder:
    def method(self, walk):
        check("method", walk)

    @classmethod
    def class_method(cls, walk):
        check("classmethod", walk)

    @staticmethod
    def static_method(walk):
        check("staticmethod", walk)


def in_a_lambda(walk):
    (lambda: check("lambda", walk))()


def in_a_comprehension(walk):
    # A comprehension has its own code object on every supported version.
    return [check("comprehension", walk) for _ in range(1)]


def in_a_generator(walk):
    def gen():
        yield check("generator mid-yield", walk)
    list(gen())


def in_an_except_block(walk):
    try:
        raise ValueError("expected")
    except ValueError:
        check("except block", walk)


def below_a_c_call(walk):
    """A frame entered from C, which sits under an interpreter shim frame."""
    def key(item):
        check("below a C call (sorted key=)", walk)
        return item
    sorted([1], key=key)


def in_async_code(walk):
    async def coro():
        check("async def under asyncio.run", walk)
    asyncio.run(coro())


def deep_recursion(walk, depth):
    """Well past the frame cap, to exercise truncation rather than a hang."""
    def recurse(n):
        if n == 0:
            walked = walk()
            if not walked:
                failures.append("deep recursion: walker returned no frames")
            elif len(walked) > 600:
                failures.append(
                    f"deep recursion: walker returned {len(walked)} frames, "
                    "above the 600 frame cap"
                )
            return
        recurse(n - 1)

    limit = sys.getrecursionlimit()
    sys.setrecursionlimit(max(limit, depth + 200))
    try:
        recurse(depth)
    finally:
        sys.setrecursionlimit(limit)
    global checks
    checks += 1


def main():
    try:
        from pyroscope import _native
    except ImportError as exc:
        sys.exit(f"could not import pyroscope._native: {exc}")

    if not hasattr(_native, "_debug_walk_stack"):
        sys.exit("this build has no _debug_walk_stack; rebuild the extension")

    report = _native._debug_offsets_selftest()
    print(f"interpreter : CPython {sys.version.split()[0]}")
    print("built for   : CPython {}.{}".format(*report["build_version"]))
    if not report["supported"]:
        print(f"unsupported : {report['error']}")
        print("\nOK (nothing to walk; memory profiling is unavailable here)")
        return

    walk = _native._debug_walk_stack

    at_module_level(walk)
    in_a_plain_function(walk)
    in_a_nested_function(walk)
    Holder().method(walk)
    Holder.class_method(walk)
    Holder.static_method(walk)
    in_a_lambda(walk)
    in_a_comprehension(walk)
    in_a_generator(walk)
    in_an_except_block(walk)
    below_a_c_call(walk)
    in_async_code(walk)
    deep_recursion(walk, 1500)

    print(f"\nchecks run  : {checks}")
    if failures:
        print("\nFAILURES:", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        sys.exit(1)
    print("OK (the walker agrees with traceback.extract_stack() everywhere)")


if __name__ == "__main__":
    main()
