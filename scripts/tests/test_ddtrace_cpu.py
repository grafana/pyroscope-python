"""End-to-end smoke test for cpu_profiler=CpuProfiler.Ddtrace.

Self-contained: it stands up a local HTTP server that captures the agent's
push requests and decodes the pprof out of them, so it needs no Pyroscope
server. Run it with the extension installed:

    python scripts/tests/test_ddtrace_cpu.py

What it checks, in order of how easy each is to break:

1. The busy function appears in the profile at all. If the thread
   auto-registration patch in cpp/cpu/ddtrace_stack/src/echion/threads.cc were
   dropped, echion's thread_info_map would stay empty and the profile would be
   completely blank.
2. Reported CPU does not exceed what the process could possibly have used.
   This is the wall-clock-vs-CPU weighting trap: the sampler walks *every*
   Python thread each tick, so crediting each tick a full sampling period
   inflates the total by roughly the thread count.
3. Reported CPU is in the right ballpark versus os.times(), so the previous
   check cannot be satisfied by reporting nothing.
4. The same holds under thread churn, which is what the recycled-pthread_t half
   of the threads.cc patch guards: a new thread inheriting a dead thread's
   pthread_t must not keep the dead thread's CPU clock.
5. Selecting the profiler where it cannot work (non-Linux, or CPython 3.10)
   raises instead of silently falling back to py-spy.
"""

import gzip
import hashlib
import logging
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pyroscope

logger = logging.getLogger("test_ddtrace_cpu")

APP_NAME = "pyroscopers.python.test.ddtrace_cpu"
SAMPLE_RATE = 100
UPLOAD_INTERVAL = 1


# --------------------------------------------------------------------------
# Minimal protobuf / pprof reader.
#
# Hand-rolled so the test has no dependency beyond the standard library. It
# only understands the handful of fields it needs; everything else is skipped.
# --------------------------------------------------------------------------

def _read_varint(buf, pos):
    result = 0
    shift = 0
    while True:
        byte = buf[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if not byte & 0x80:
            return result, pos
        shift += 7


def _iter_fields(buf):
    """Yield (field_number, wire_type, value) for each field in `buf`.

    `value` is bytes for wire type 2 and an int otherwise.
    """
    pos = 0
    end = len(buf)
    while pos < end:
        key, pos = _read_varint(buf, pos)
        field, wire = key >> 3, key & 7
        if wire == 0:
            value, pos = _read_varint(buf, pos)
        elif wire == 1:
            value = int.from_bytes(buf[pos:pos + 8], "little")
            pos += 8
        elif wire == 2:
            length, pos = _read_varint(buf, pos)
            value = buf[pos:pos + length]
            pos += length
        elif wire == 5:
            value = int.from_bytes(buf[pos:pos + 4], "little")
            pos += 4
        else:
            raise ValueError("unsupported wire type {}".format(wire))
        yield field, wire, value


def _packed_varints(buf):
    pos = 0
    out = []
    while pos < len(buf):
        value, pos = _read_varint(buf, pos)
        out.append(value)
    return out


def _zigzag_free_int64(values):
    """pprof int64 fields are plain varints, sign-extended at 64 bits."""
    return [v - (1 << 64) if v >= (1 << 63) else v for v in values]


class Profile:
    """The slice of a pprof Profile this test cares about."""

    def __init__(self, raw):
        self.strings = []
        self.functions = {}   # function id -> name string index
        self.locations = {}   # location id -> [function id]
        self.samples = []     # (location ids leaf-first, values)
        self.sample_types = []

        for field, _wire, value in _iter_fields(raw):
            if field == 1:      # sample_type
                self.sample_types.append(self._value_type(value))
            elif field == 2:    # sample
                self.samples.append(self._sample(value))
            elif field == 4:    # location
                loc_id, func_ids = self._location(value)
                self.locations[loc_id] = func_ids
            elif field == 5:    # function
                func_id, name_idx = self._function(value)
                self.functions[func_id] = name_idx
            elif field == 6:    # string_table
                self.strings.append(value.decode("utf-8", "replace"))

    @staticmethod
    def _value_type(buf):
        type_idx = unit_idx = 0
        for field, _wire, value in _iter_fields(buf):
            if field == 1:
                type_idx = value
            elif field == 2:
                unit_idx = value
        return type_idx, unit_idx

    @staticmethod
    def _sample(buf):
        location_ids = []
        values = []
        for field, wire, value in _iter_fields(buf):
            if field == 1:
                location_ids.extend(_packed_varints(value) if wire == 2 else [value])
            elif field == 2:
                raw = _packed_varints(value) if wire == 2 else [value]
                values.extend(_zigzag_free_int64(raw))
        return location_ids, values

    @staticmethod
    def _location(buf):
        loc_id = 0
        func_ids = []
        for field, _wire, value in _iter_fields(buf):
            if field == 1:
                loc_id = value
            elif field == 4:  # line
                for lf, _lw, lv in _iter_fields(value):
                    if lf == 1:
                        func_ids.append(lv)
        return loc_id, func_ids

    @staticmethod
    def _function(buf):
        func_id = name_idx = 0
        for field, _wire, value in _iter_fields(buf):
            if field == 1:
                func_id = value
            elif field == 2:
                name_idx = value
        return func_id, name_idx

    def function_names(self, location_ids):
        names = []
        for loc_id in location_ids:
            for func_id in self.locations.get(loc_id, ()):
                names.append(self.strings[self.functions.get(func_id, 0)])
        return names

    def total_value(self):
        return sum(values[0] for _locs, values in self.samples if values)

    def value_for_function(self, name):
        total = 0
        for locs, values in self.samples:
            if values and name in self.function_names(locs):
                total += values[0]
        return total

    def sample_type_names(self):
        return [
            (self.strings[t], self.strings[u]) for t, u in self.sample_types
        ]


def decode_push_request(body):
    """Return [(labels dict, Profile)] from a gzipped PushRequest."""
    raw = gzip.decompress(body)
    out = []
    for field, _wire, value in _iter_fields(raw):
        if field != 1:  # series
            continue
        labels = {}
        for sf, _sw, sv in _iter_fields(value):
            if sf == 1:  # LabelPair
                name = val = ""
                for lf, _lw, lv in _iter_fields(sv):
                    if lf == 1:
                        name = lv.decode()
                    elif lf == 2:
                        val = lv.decode()
                labels[name] = val
            elif sf == 2:  # RawSample
                for rf, _rw, rv in _iter_fields(sv):
                    if rf == 1:
                        out.append((labels, Profile(rv)))
    return out


# --------------------------------------------------------------------------
# Capture server
# --------------------------------------------------------------------------

class Collector:
    def __init__(self):
        self.lock = threading.Lock()
        self.profiles = []  # (labels, Profile)

    def add(self, entries):
        with self.lock:
            self.profiles.extend(entries)

    def cpu_profiles(self):
        with self.lock:
            return [
                (labels, profile)
                for labels, profile in self.profiles
                if labels.get("__name__") == "process_cpu"
            ]

    def reset(self):
        with self.lock:
            self.profiles = []


def make_server(collector):
    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get("Content-Length", 0))
            body = self.rfile.read(length)
            try:
                collector.add(decode_push_request(body))
            except Exception:
                logger.exception("failed to decode push request")
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server, thread


# --------------------------------------------------------------------------
# Workload
# --------------------------------------------------------------------------

def cpuhog(stop):
    """Burn CPU in a distinctively named frame."""
    value = "seed"
    while not stop.is_set():
        for _ in range(2000):
            value = hashlib.sha256(value.encode()).hexdigest()
    return value


def process_cpu_seconds():
    times = os.times()
    return times.user + times.system


def run_workload(nthreads, duration, churn=False):
    """Run `nthreads` busy threads for `duration` seconds.

    With `churn`, threads are short-lived and constantly replaced, so pthread_t
    values get recycled.
    """
    stop = threading.Event()
    cpu_before = process_cpu_seconds()
    wall_before = time.monotonic()

    if not churn:
        threads = [
            threading.Thread(target=cpuhog, args=(stop,), name="cpuhog-%d" % i)
            for i in range(nthreads)
        ]
        for t in threads:
            t.start()
        time.sleep(duration)
        stop.set()
        for t in threads:
            t.join()
    else:
        deadline = time.monotonic() + duration
        while time.monotonic() < deadline:
            round_stop = threading.Event()
            threads = [
                threading.Thread(target=cpuhog, args=(round_stop,), name="churn-%d" % i)
                for i in range(nthreads)
            ]
            for t in threads:
                t.start()
            time.sleep(0.3)
            round_stop.set()
            for t in threads:
                t.join()

    return process_cpu_seconds() - cpu_before, time.monotonic() - wall_before


# --------------------------------------------------------------------------
# Assertions
# --------------------------------------------------------------------------

def check_profiles(collector, label, actual_cpu_s, wall_s, nthreads):
    profiles = collector.cpu_profiles()
    if not profiles:
        raise AssertionError("%s: no process_cpu profiles were pushed" % label)

    reported_ns = sum(p.total_value() for _labels, p in profiles)
    cpuhog_ns = sum(p.value_for_function("cpuhog") for _labels, p in profiles)

    reported_s = reported_ns / 1e9
    logger.info(
        "%s: %d profiles, reported %.2fs CPU (%.2fs in cpuhog), "
        "process used %.2fs over %.2fs wall with %d busy threads",
        label, len(profiles), reported_s, cpuhog_ns / 1e9,
        actual_cpu_s, wall_s, nthreads,
    )

    # 1. The busy frame has to be there at all.
    if cpuhog_ns == 0:
        names = set()
        for _labels, p in profiles:
            for locs, _values in p.samples:
                names.update(p.function_names(locs))
        raise AssertionError(
            "%s: 'cpuhog' never appeared in the profile; saw %s" % (label, sorted(names)[:20])
        )

    # 2. Values must be CPU nanoseconds, not one sampling period per thread per
    #    tick. The ceiling is generous (2x) so this only fires on the real bug,
    #    which overshoots by roughly the thread count.
    ceiling_s = actual_cpu_s * 2 + 1.0
    if reported_s > ceiling_s:
        raise AssertionError(
            "%s: reported %.2fs of CPU but the process only used %.2fs. "
            "Samples are probably weighted by the sampling period instead of "
            "the per-thread CPU delta -- see the sample weighting section of "
            "cpp/cpu/ddtrace_stack/VENDOR.md" % (label, reported_s, actual_cpu_s)
        )

    # 3. ...and it must not be satisfied by reporting almost nothing. A
    #    sampler that only sees a fraction of the threads (the recycled
    #    pthread_t bug reported ~5%) fails here.
    floor_s = actual_cpu_s * 0.25
    if reported_s < floor_s:
        raise AssertionError(
            "%s: reported only %.2fs of CPU for a process that used %.2fs. "
            "Threads are probably being skipped or billed against a dead CPU "
            "clock -- see the threads.cc patch in "
            "cpp/cpu/ddtrace_stack/VENDOR.md" % (label, reported_s, actual_cpu_s)
        )

    types = profiles[0][1].sample_type_names()
    logger.info("%s: sample types %s", label, types)


def check_unsupported_raises():
    """Selecting an unbuilt profiler must raise, not fall back to py-spy."""
    try:
        pyroscope.configure(
            application_name=APP_NAME,
            server_address="http://127.0.0.1:1",
            cpu_profiler=pyroscope.CpuProfiler.Ddtrace,
        )
    except RuntimeError as e:
        logger.info("unsupported platform raised as expected: %s", e)
        return True
    pyroscope.shutdown()
    return False


def main():
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")

    supported = sys.platform == "linux" and sys.version_info >= (3, 11)
    if not supported:
        # cpu_profiler=Ddtrace is Linux/CPython 3.11+ only today; assert that it
        # says so rather than silently profiling with py-spy.
        if not check_unsupported_raises():
            raise AssertionError(
                "cpu_profiler=Ddtrace is not supported on %s/%d.%d but configure() "
                "accepted it instead of raising"
                % (sys.platform, sys.version_info[0], sys.version_info[1])
            )
        logger.info("done (unsupported here: only the rejection path is exercised)")
        return

    collector = Collector()
    server, _thread = make_server(collector)
    address = "http://127.0.0.1:%d" % server.server_address[1]
    logger.info("collector listening on %s", address)

    nthreads = 4
    pyroscope.configure(
        application_name=APP_NAME,
        server_address=address,
        enable_logging=True,
        sample_rate=SAMPLE_RATE,
        upload_interval=UPLOAD_INTERVAL,
        cpu_profiler=pyroscope.CpuProfiler.Ddtrace,
        report_pid=True,
        report_thread_id=True,
        mem_enabled=False,
    )

    try:
        cpu_s, wall_s = run_workload(nthreads, duration=6)
        # Let the last upload interval land.
        time.sleep(UPLOAD_INTERVAL * 3)
        check_profiles(collector, "steady", cpu_s, wall_s, nthreads)

        collector.reset()
        cpu_s, wall_s = run_workload(nthreads, duration=6, churn=True)
        time.sleep(UPLOAD_INTERVAL * 3)
        check_profiles(collector, "churn", cpu_s, wall_s, nthreads)
    finally:
        pyroscope.shutdown()
        server.shutdown()

    logger.info("done")


if __name__ == "__main__":
    main()
