"""Local ingest sink.

Terminates the upload path on the benchmark machine so network and server cost
stay out of the numbers, while still exercising the agent's full encode +
compress + POST path.

Two rules shape this file:

* Decode *after* the run, never during it. The handler does the minimum work to
  return 200 and stores the body; parsing happens once the measured region is
  over. Decoding inline would put protobuf work on the critical path and, worse,
  could slow the POST enough to back-pressure the agent through its
  sync_channel(10) upload queue, moving cost out of the column being measured.
* Partition by run_id. The sink outlives individual runs, so counts must be
  attributable to one run or a previous run contaminates the next one's checks.
"""

import argparse
import gzip
import json
import threading
import time
import traceback
from collections import defaultdict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from pprof import profile_pb2, push_pb2

PUSH_PATH = "/push.v1.PusherService/Push"


class _Store:
    def __init__(self):
        self.lock = threading.Lock()
        # run_id -> list of (raw_body_bytes, post_duration_s)
        self.bodies = defaultdict(list)

    def add(self, body, duration):
        with self.lock:
            self.bodies["__pending__"].append((body, duration))

    def take_all(self):
        with self.lock:
            pending = self.bodies.pop("__pending__", [])
            self.bodies.clear()
        return pending


STORE = _Store()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        start = time.monotonic()
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length) if length else b""
        ok = self.path.endswith(PUSH_PATH)
        self.send_response(200 if ok else 404)
        self.send_header("Content-Length", "0")
        self.end_headers()
        if ok:
            STORE.add(body, time.monotonic() - start)

    def do_GET(self):
        if self.path == "/-/drain":
            # A decode failure must come back as a readable error. Letting it
            # propagate kills the handler thread and the caller sees only a
            # closed connection, which says nothing about what went wrong.
            try:
                payload = json.dumps(decode(STORE.take_all())).encode()
            except Exception:
                payload = json.dumps(
                    {"__sink_error__": traceback.format_exc()}).encode()
        elif self.path == "/-/ready":
            payload = b'{"ready":true}'
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *_args):
        pass  # the default handler writes to stderr on every request


def decode(entries):
    """Turn stored bodies into per-run_id totals.

    Sums sample *values*, not the number of Sample records. A sampler that
    aggregates identical stacks emits far fewer records for the same amount of
    collected work, so counting records measures cardinality rather than volume.

    The agent's CPU sample_type is nanoseconds (value = count * period, with
    period = 1e9 / sample_rate), so the summed value converts straight to the
    CPU seconds the profile claims to have observed.
    """
    per_run = defaultdict(lambda: {
        "profiles": 0,
        "samples": 0,
        "sample_value_ns": 0,
        "upload_bytes": 0,
        "post_durations": [],
        "profile_types": defaultdict(int),
        "functions": defaultdict(int),
    })

    for body, duration in entries:
        req = push_pb2.PushRequest()
        req.ParseFromString(gzip.decompress(body))
        for series in req.series:
            labels = {lp.name: lp.value for lp in series.labels}
            run_id = labels.get("run_id", "__unlabelled__")
            acc = per_run[run_id]
            acc["upload_bytes"] += len(body)
            acc["post_durations"].append(duration)
            for raw in series.samples:
                prof = profile_pb2.Profile()
                prof.ParseFromString(raw.raw_profile)
                acc["profiles"] += 1
                acc["profile_types"][labels.get("__name__", "?")] += 1
                strings = list(prof.string_table)
                funcs = {f.id: strings[f.name] for f in prof.function}
                locs = {
                    l.id: (funcs.get(l.line[0].function_id, "?") if l.line else "?")
                    for l in prof.location
                }
                for s in prof.sample:
                    acc["samples"] += 1
                    if s.value:
                        acc["sample_value_ns"] += s.value[0]
                        # Leaf frame, for the attribution cross-checks.
                        if s.location_id:
                            leaf = locs.get(s.location_id[0], "?")
                            acc["functions"][leaf] += s.value[0]

    out = {}
    for run_id, acc in per_run.items():
        posts = acc["post_durations"]
        out[run_id] = {
            "profiles": acc["profiles"],
            "samples": acc["samples"],
            "seen_cpu_s": acc["sample_value_ns"] / 1e9,
            "upload_bytes": acc["upload_bytes"],
            "max_post_s": max(posts) if posts else 0.0,
            "profile_types": dict(acc["profile_types"]),
            "top_functions": dict(
                sorted(acc["functions"].items(), key=lambda kv: -kv[1])[:10]
            ),
        }
    return out


def serve(port):
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.daemon_threads = True
    return server


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=4040)
    args = ap.parse_args()
    server = serve(args.port)
    print(f"sink listening on http://127.0.0.1:{server.server_address[1]}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
