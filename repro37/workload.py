"""Interpreter churn designed to maximize what the profiler has to read out of
a *live, mutating* interpreter.

py-spy samples this process by walking its own memory (process_vm_readv) without
stopping it, so every pointer it follows -- thread state -> frame -> code object
-> name/filename string -- can be freed and its memory reused between the read
that produced the pointer and the read that dereferences it.

The knobs that matter here:
  * frames are created and destroyed as fast as possible (deep recursion),
  * code objects are compiled and dropped constantly, so the addresses being
    dereferenced go stale,
  * function names and filenames are non-BMP (astral) text. CPython stores
    those as UCS-4, which is the one string kind py-spy decodes by
    reinterpreting the bytes it read as `char` values -- a garbage/torn read
    there produces invalid `char`s,
  * freed memory is immediately reused by random bytes, so a stale read returns
    adversarial garbage rather than a still-intact object,
  * threads come and go, so OS thread ids get recycled under the sampler.
"""
import faulthandler
import os
import random
import string
import sys
import threading
import time

# Deseret letters: valid Python identifier characters that live outside the BMP,
# so CPython stores these names as UCS-4 (PyUnicode kind=4).
ASTRAL_IDENT = [chr(0x10400 + i) for i in range(20)]
ASTRAL_TEXT = [chr(0x1F600 + i) for i in range(20)]


def _ident(n):
    return "".join(random.choices(ASTRAL_IDENT, k=n)) + "x"


def _filename(n):
    return "/" + "".join(random.choices(ASTRAL_TEXT, k=n)) + ".py"


def make_code(depth, name_len=64, file_len=64):
    """Compile a fresh chain of functions with astral names in an astral
    filename. Every call allocates new code objects and new UCS-4 strings and
    lets the previous ones be freed."""
    name = _ident(name_len)
    src = []
    for i in range(depth):
        callee = f"{name}_{i + 1}" if i + 1 < depth else "leaf"
        src.append(f"def {name}_{i}(n):\n    return {callee}(n)\n")
    src.append("def leaf(n):\n    return sum(range(n))\n")
    ns = {}
    exec(compile("\n".join(src), _filename(file_len), "exec"), ns)
    return ns[f"{name}_0"]


def churn(stop, depth):
    """Code object churn: build a deep chain, run it, throw it away."""
    while not stop.is_set():
        fn = make_code(depth)
        for _ in range(10):
            fn(100)
        del fn


def recurse(stop, depth):
    """Frame churn: push and pop deep stacks as fast as the interpreter allows."""
    fn = make_code(depth)
    while not stop.is_set():
        for _ in range(200):
            fn(1)


def garbage(stop):
    """Reuse freed memory with random bytes, so a stale read by the sampler
    returns garbage instead of an object that still looks intact."""
    sizes = [56, 80, 104, 152, 200, 312, 408, 520, 1032, 4104]
    while not stop.is_set():
        keep = [os.urandom(random.choice(sizes)) for _ in range(2000)]
        keep.reverse()
        del keep


def thread_churn(stop):
    """Short-lived threads, so OS thread ids get recycled while the sampler is
    iterating /proc/<pid>/task."""
    while not stop.is_set():
        ts = [threading.Thread(target=lambda: sum(range(1000))) for _ in range(8)]
        for t in ts:
            t.start()
        for t in ts:
            t.join()


def install_frame_cycle():
    """Make py-spy's frame walk fail on every sample.

    The core dump of a reproduced crash shows the victim is always an
    anyhow::Error from `Sample.sampling_errors`, so the crash rate should track
    how many of those the sampler produces and the consumer drops. A parked
    frame whose `previous` points at itself makes every walk hit py-spy's
    "Max frame recursion depth reached" bound, i.e. one error per sample.

    Same trick as poc_frame_cycle.py; see that file for the details."""
    import poc_frame_cycle as fc

    parked = threading.Event()
    ready = threading.Event()

    def inner():
        ready.set()
        parked.wait()

    def outer():
        inner()

    t = threading.Thread(target=outer, daemon=True)
    t.start()
    ready.wait()
    time.sleep(0.2)
    frame = sys._current_frames()[t.ident]
    inner_if = fc.iframe_of(frame)
    outer_if = fc.iframe_of(frame.f_back)
    off = fc.find_previous_offset(inner_if, outer_if)
    fc.set_word(inner_if + off, inner_if)
    return t


def run(duration, nthreads, depth):
    if os.environ.get("FRAME_CYCLE") == "1":
        install_frame_cycle()
    stop = threading.Event()
    targets = []
    for i in range(nthreads):
        kind = i % 4
        if kind == 0:
            targets.append((churn, (stop, depth)))
        elif kind == 1:
            targets.append((recurse, (stop, depth)))
        elif kind == 2:
            targets.append((garbage, (stop,)))
        else:
            targets.append((thread_churn, (stop,)))
    threads = []
    for target, args in targets:
        t = threading.Thread(target=target, args=args, daemon=True)
        t.start()
        threads.append(t)
    deadline = time.time() + duration
    while time.time() < deadline:
        time.sleep(0.2)
    stop.set()
    for t in threads:
        t.join(timeout=10)


def _flag(name, default):
    return os.environ.get(name, default) == "1"


def configure():
    """All knobs come from the environment so the same workload can be run
    across the profiler's configurations (the defaults are what the docs tell
    people to use; the non-default ones exercise more of the sampler).

    NO_PROFILER=1 runs the identical workload with no agent at all, as a
    control for whether the churn alone can corrupt the heap."""
    if os.environ.get("NO_PROFILER") == "1":
        print("control run: profiler NOT started", file=sys.stderr, flush=True)
        return
    import pyroscope

    pyroscope.configure(
        application_name="repro37",
        server_address=os.environ.get("PYROSCOPE_SERVER", "http://127.0.0.1:4040"),
        sample_rate=int(os.environ.get("SAMPLE_RATE", "997")),
        oncpu=_flag("ONCPU", "1"),
        gil_only=_flag("GIL_ONLY", "1"),
        report_pid=_flag("REPORT_PID", "0"),
        report_thread_id=_flag("REPORT_THREAD_ID", "0"),
        report_thread_name=_flag("REPORT_THREAD_NAME", "0"),
        upload_interval=int(os.environ.get("UPLOAD_INTERVAL", "5")),
        mem_enabled=_flag("MEM_ENABLED", "0"),
        mem_heap_sample_size=int(os.environ.get("MEM_SAMPLE_SIZE", str(512 * 1024))),
        mem_max_nframe=int(os.environ.get("MEM_MAX_NFRAME", "128")),
        mem_enable_mem_domain=_flag("MEM_DOMAIN", "1"),
        cpu_enabled=_flag("CPU_ENABLED", "1"),
    )


if __name__ == "__main__":
    if os.environ.get("FAULTHANDLER", "1") == "1":
        # Matches what the reports show: a native fault prints the Python-level
        # stacks of every thread before the process dies.
        faulthandler.enable()
    configure()
    run(
        float(os.environ.get("DURATION", "60")),
        int(os.environ.get("THREADS", "8")),
        int(os.environ.get("DEPTH", "40")),
    )
    print("workload finished cleanly", file=sys.stderr)
    os._exit(0)  # a parked thread cannot be joined when FRAME_CYCLE is on
