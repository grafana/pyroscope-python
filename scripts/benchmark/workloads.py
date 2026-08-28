"""Workload shapes.

Every shape is fixed-work: it runs an exact number of iterations and returns the
count it completed. A fixed-duration workload would hide the profiler's cost --
the profiler steals time and the workload simply does less, so the overhead
lands in a metric nobody is looking at.

Each shape exists to expose something the others cannot. The current profiler is
py-spy sampling its own process at sample_rate Hz. The benchmark measures it at
gil_only=False, where a tick unwinds every live Python thread, so the axes that
matter are thread count and stack depth. (At the shipping gil_only=True, py-spy
skips non-GIL threads *before* unwinding, and only one stack is walked per tick.)
"""

import hashlib
import socket
import threading
import time

# Stack depth of the innermost work function, counted from the thread entry
# point. The sampler unwinds every frame, so depth is the dominant lever on its
# per-tick cost; `deep` is far enough past the baseline to separate the two.
# Both sit well under sys.getrecursionlimit() (1000) with room for the threading
# and harness frames above them.
SHALLOW_DEPTH = 128
DEEP_DEPTH = 256

# How long an IO thread stays blocked per iteration. Sized so the block, not the
# CPU slice, dominates the iteration -- otherwise the "IO-bound" shape is really
# a CPU shape wearing a socket.
IO_WAIT_S = 0.002

# Work per iteration, per shape. The IO shapes get a much smaller slice so the
# blocking wait is what the thread spends its time doing.
INNER_SCALE = {
    "io_bound": 0.05,
}


def _burn(n):
    """One unit of CPU work. Pure Python plus a hash, no allocation storm."""
    h = hashlib.sha256()
    acc = 0
    for i in range(n):
        h.update(b"pyroscope-benchmark")
        acc = (acc + i * i) % 1000003
    return acc + h.digest()[0]


def _nest(depth, fn, *args):
    """Call fn under `depth` extra Python frames.

    Recursion is the honest way to build depth here: the sampler unwinds real
    frames, so synthesised depth has to be real frames too.
    """
    if depth <= 0:
        return fn(*args)
    return _nest(depth - 1, fn, *args)


# --- individual thread bodies ------------------------------------------------

def _cpu_thread(iterations, inner, depth, counter, idx):
    done = 0
    for _ in range(iterations):
        _nest(depth, _burn, inner)
        done += 1
    counter[idx] = done


def _io_thread(iterations, inner, depth, sock, wait_s, counter, idx):
    """A small CPU slice, a real socket round-trip, then a real block.

    The socket round-trip is there because a server actually does one, and it
    exercises the GIL release/reacquire around a syscall. The explicit wait is
    what makes the shape genuinely IO-bound: a localhost round-trip returns in
    tens of microseconds, which is not enough to stop the CPU slice from
    dominating, and a shape that is secretly CPU-bound proves nothing about how
    the sampler treats blocked threads.

    time.sleep releases the GIL and consumes no CPU, which is exactly the
    property under test.
    """
    done = 0
    payload = b"x" * 64
    for _ in range(iterations):
        _nest(depth, _burn, inner)
        sock.sendall(payload)
        got = 0
        while got < len(payload):
            chunk = sock.recv(len(payload) - got)
            if not chunk:
                raise RuntimeError("io_bound: echo peer closed early")
            got += len(chunk)
        if wait_s:
            time.sleep(wait_s)
        done += 1
    counter[idx] = done


class _EchoServer:
    """Localhost echo peer for the IO shapes.

    Runs in this process on its own threads. That is deliberate: it keeps the
    IO shapes self-contained, and its threads are themselves part of what the
    sampler has to walk, which is exactly the cost we want represented.
    """

    def __init__(self):
        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(64)
        self.address = self._listener.getsockname()
        self._stop = threading.Event()
        self._threads = []
        self._accept_thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._accept_thread.start()

    def _accept_loop(self):
        while not self._stop.is_set():
            try:
                conn, _ = self._listener.accept()
            except OSError:
                return
            t = threading.Thread(target=self._echo, args=(conn,), daemon=True)
            t.start()
            # Drop finished handlers so the list does not grow across a long
            # run, and so the live count reflects the threads the sampler
            # actually has to walk.
            self._threads = [x for x in self._threads if x.is_alive()]
            self._threads.append(t)

    def _echo(self, conn):
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        with conn:
            while not self._stop.is_set():
                try:
                    data = conn.recv(4096)
                except OSError:
                    return
                if not data:
                    return
                conn.sendall(data)

    def connect(self):
        s = socket.create_connection(self.address)
        s.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        return s

    def close(self):
        self._stop.set()
        self._listener.close()


# --- shapes ------------------------------------------------------------------

def _run_threads(specs):
    """Start every thread, join every thread, return total completed iterations.

    A thread that raises must fail the whole run. Swallowing it would just lower
    the completed-iteration count, which looks like less work rather than like a
    bug -- exactly what fixed-work measurement is supposed to make impossible.
    """
    counter = [0] * len(specs)
    errors = []

    def guarded(fn, args, idx):
        try:
            fn(*args, counter, idx)
        except BaseException as exc:  # noqa: BLE001 - re-raised on the main thread
            errors.append(exc)

    threads = [
        threading.Thread(target=guarded, args=(fn, args, i))
        for i, (fn, args) in enumerate(specs)
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    if errors:
        raise errors[0]
    return sum(counter)


def cpu_single(iterations, inner, **_):
    return _run_threads([(_cpu_thread, (iterations, inner, SHALLOW_DEPTH))])


def cpu_multi(iterations, inner, threads=4, **_):
    """`iterations` is the per-thread count, so total work scales with threads.

    Keeping per-thread work fixed is what makes the thread-count sweep readable:
    each added thread adds one stack walk per tick *and* a proportional amount of
    application work, so the overhead percentage is comparable across N.
    """
    return _run_threads(
        [(_cpu_thread, (iterations, inner, SHALLOW_DEPTH)) for _ in range(threads)]
    )


def deep(iterations, inner, **_):
    """Deep stack, rebuilt every iteration: descend, work, return, repeat.

    This is the shape that grows and shrinks the frame stack past a CPython
    datastack chunk boundary on every pass, which is what makes the sampler drop
    samples. Kept as-is because that is a real workload shape -- recursive
    parsers and tree walks do exactly this -- and the loss is worth measuring.
    """
    return _run_threads([(_cpu_thread, (iterations, inner, DEEP_DEPTH))])


def _deep_stable_thread(iterations, inner, depth, counter, idx):
    def work_at_bottom():
        done = 0
        for _ in range(iterations):
            _burn(inner)
            done += 1
        return done

    counter[idx] = _nest(depth, work_at_bottom)


def deep_stable(iterations, inner, **_):
    """The same depth, held constant: descend once, do all the work at the bottom.

    Isolates the cost of unwinding a deep stack from the cost of a stack that is
    continuously rebuilt. Without this shape, `deep` alone cannot distinguish "a
    deep stack is expensive to unwind" from "a churning deep stack is not
    sampled at all", and the two have opposite implications.
    """
    return _run_threads([(_deep_stable_thread, (iterations, inner, DEEP_DEPTH))])


def io_bound(iterations, inner, threads=4, server=None, io_wait_s=IO_WAIT_S, **_):
    socks = [server.connect() for _ in range(threads)]
    try:
        return _run_threads(
            [
                (_io_thread, (iterations, inner, SHALLOW_DEPTH, socks[i], io_wait_s))
                for i in range(threads)
            ]
        )
    finally:
        for s in socks:
            s.close()


def mixed(iterations, inner, threads=4, server=None, io_wait_s=IO_WAIT_S,
          io_inner=None, **_):
    """Half CPU-bound, half IO-bound, running concurrently.

    The point is attribution: the profile should credit the CPU threads and not
    the blocked ones, while the sampler still pays to walk both.

    The two halves get different slice sizes on purpose. Giving the CPU threads
    the IO threads' tiny slice would make the whole shape low-duty-cycle, and a
    baseline that consumes almost no CPU has a noise floor wide enough to hide
    any overhead being measured.
    """
    n_io = threads // 2
    n_cpu = threads - n_io
    if io_inner is None:
        io_inner = max(1, int(inner * INNER_SCALE["io_bound"]))
    socks = [server.connect() for _ in range(n_io)]
    try:
        specs = [(_cpu_thread, (iterations, inner, SHALLOW_DEPTH)) for _ in range(n_cpu)]
        specs += [
            (_io_thread, (iterations, io_inner, SHALLOW_DEPTH, socks[i], io_wait_s))
            for i in range(n_io)
        ]
        return _run_threads(specs)
    finally:
        for s in socks:
            s.close()


def churn(iterations, inner, threads=4, batch=8, **_):
    """Short-lived threads, created and destroyed continuously.

    Total work is still fixed: `iterations` units spread over waves of throwaway
    threads doing `batch` units each, sized by the calibrator for roughly a 50 ms
    lifetime. This stresses per-tick task-table enumeration and any per-thread
    state the sampler caches against a pthread_t that later gets recycled.
    """
    batch = max(1, batch)
    remaining = iterations
    done = 0
    while remaining > 0:
        wave = []
        for _ in range(threads):
            if remaining <= 0:
                break
            n = min(batch, remaining)
            wave.append((_cpu_thread, (n, inner, SHALLOW_DEPTH)))
            remaining -= n
        done += _run_threads(wave)
    return done


SHAPES = {
    "cpu_single": cpu_single,
    "cpu_multi": cpu_multi,
    "io_bound": io_bound,
    "mixed": mixed,
    "deep": deep,
    "deep_stable": deep_stable,
    "churn": churn,
}

# Shapes that need the in-process echo peer.
NEEDS_SERVER = {"io_bound", "mixed"}

# Shapes that run their work on a single thread. Their `iterations` argument is
# therefore the total, not a per-thread count.
SINGLE_THREADED = {"cpu_single", "deep", "deep_stable"}

# Shapes whose `iterations` argument is the total across the whole run rather
# than per thread. `churn` belongs here despite using several threads, because it
# spreads one fixed budget of work over waves of short-lived ones.
#
# This is declared once and imported by the runner. It used to be duplicated
# there, and adding a shape to one copy but not the other made the harness expect
# four times the work the shape actually performs.
TOTAL_ITERATION_SHAPES = SINGLE_THREADED | {"churn"}


def expected_iterations(shape, iterations, threads):
    """How many iterations a correct run of this shape must complete."""
    if shape in TOTAL_ITERATION_SHAPES:
        return iterations
    return iterations * threads


# Total Python threads alive during the measured region, excluding the echo
# server's own threads. Used to state what a tick actually costs.
def worker_threads(shape, threads):
    if shape in SINGLE_THREADED:
        return 1
    return threads


def make_server():
    return _EchoServer()
