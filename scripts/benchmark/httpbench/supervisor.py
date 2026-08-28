"""Runs inside the measured cgroup scope and drives one HTTP benchmark run.

The split of responsibilities matters. This process lives inside the scope, so
it can read the scope's own cpu.stat and memory.peak, and gunicorn -- master and
workers -- inherits that cgroup, which is what makes the multi-process shape
measurable with the same mechanism as the single-process shapes.

The load generator is deliberately *not* started here: it must run outside this
cgroup or its CPU would be counted against the server. The orchestrator launches
it in its own scope on a disjoint CPU set and drives this process through a tiny
control endpoint. That endpoint handles four requests over a whole run, so its
cost is negligible and, being identical across variants, cancels out anyway.
"""

import json
import os
import signal
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from cgroup import CgroupCounters
from protocol import RESULT_MARKER

HERE = os.path.dirname(os.path.abspath(__file__))


class Supervisor:
    def __init__(self, args):
        self.args = args
        self.counters = CgroupCounters()
        self.proc = None
        self.marks = {}
        self.worker_pids = set()
        self.done = threading.Event()
        # Scope creation to teardown: the window the agent is observing.
        self.t0 = time.monotonic()

    # --- gunicorn lifecycle
    def start_server(self):
        env = os.environ.copy()
        env.update({
            "BENCH_BIND": f"127.0.0.1:{self.args.port}",
            "BENCH_WORKERS": str(self.args.workers),
            "BENCH_THREADS": str(self.args.threads),
            "BENCH_VARIANT": self.args.variant,
            "BENCH_RUN_ID": self.args.run_id,
            "BENCH_SINK_URL": self.args.sink_url,
            "PYTHONPATH": os.path.dirname(HERE),
        })
        if self.args.sample_rate:
            env["BENCH_SAMPLE_RATE"] = str(self.args.sample_rate)
        if self.args.upload_interval:
            env["BENCH_UPLOAD_INTERVAL"] = str(self.args.upload_interval)

        self.proc = subprocess.Popen(
            [self.args.gunicorn, "-c", os.path.join(HERE, "gunicorn_conf.py"),
             "httpbench.app:app"],
            cwd=os.path.dirname(HERE), env=env,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )

    def wait_ready(self, timeout=60):
        """Wait until every worker has forked and served a request.

        Polling until `workers` distinct pids have answered is what proves
        post_fork ran in each of them -- a worker that never started the agent
        would otherwise just look inexpensive.
        """
        deadline = time.monotonic() + timeout
        url = f"http://127.0.0.1:{self.args.port}/healthz"
        while time.monotonic() < deadline:
            if self.proc.poll() is not None:
                raise RuntimeError(
                    f"gunicorn exited early with {self.proc.returncode}")
            try:
                with urllib.request.urlopen(url, timeout=1) as r:
                    self.worker_pids.add(json.loads(r.read())["pid"])
            except (urllib.error.URLError, OSError, json.JSONDecodeError):
                time.sleep(0.1)
                continue
            if len(self.worker_pids) >= self.args.workers:
                return
        raise RuntimeError(
            f"only {len(self.worker_pids)} of {self.args.workers} workers "
            "answered /healthz before the timeout")

    def stop_server(self):
        """Graceful stop so worker_exit runs shutdown() and flushes the tail."""
        if self.proc is None or self.proc.poll() is not None:
            return
        self.proc.send_signal(signal.SIGTERM)
        try:
            self.proc.wait(timeout=90)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait(timeout=10)

    # --- control endpoint
    def serve_control(self):
        sup = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def _reply(self, obj):
                payload = json.dumps(obj).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def do_GET(self):
                if self.path.startswith("/mark/"):
                    name = self.path.split("/", 2)[2]
                    sup.marks[name] = sup.counters.snapshot()
                    sup.marks[name]["wall"] = time.monotonic()
                    self._reply(sup.marks[name])
                elif self.path == "/ready":
                    self._reply({"workers": sorted(sup.worker_pids)})
                elif self.path == "/stop":
                    sup.stop_server()
                    sup.marks["final"] = sup.counters.snapshot()
                    sup.marks["final"]["wall"] = time.monotonic()
                    self._reply(sup.marks["final"])
                    sup.done.set()
                else:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()

            def log_message(self, *_a):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", self.args.control_port), Handler)
        server.daemon_threads = True
        threading.Thread(target=server.serve_forever, daemon=True).start()
        return server

    def run(self):
        control = self.serve_control()
        self.start_server()
        try:
            self.wait_ready()
            print("SUPERVISOR_READY", flush=True)
            # Held open by the orchestrator until it has finished the load
            # phases and asked for /stop.
            if not self.done.wait(timeout=self.args.max_run_s):
                raise RuntimeError("orchestrator never asked the run to stop")
        finally:
            self.stop_server()
            control.shutdown()

        start, end = self.marks.get("start"), self.marks.get("end")
        final = self.marks.get("final", end)
        result = {
            "shape": "http",
            "variant": self.args.variant,
            "run_id": self.args.run_id,
            "workers": self.args.workers,
            "threads_configured": self.args.threads,
            "worker_pids": sorted(self.worker_pids),
            "gunicorn_returncode": self.proc.returncode,
        }
        if start and end:
            result.update({
                "cpu_s": (end["usage_usec"] - start["usage_usec"]) / 1e6,
                "cpu_user_s": (end["user_usec"] - start["user_usec"]) / 1e6,
                "cpu_system_s": (end["system_usec"] - start["system_usec"]) / 1e6,
                "wall_s": end["wall"] - start["wall"],
                "mem_static_bytes": start["memory_current"],
                "mem_end_bytes": end["memory_current"],
                "mem_peak_bytes": final["memory_peak"],
                # The agent runs from post_fork until worker_exit, so its
                # claimed CPU is bounded against the whole process lifetime.
                "profiled_cpu_s": final["usage_usec"] / 1e6,
                "profiled_wall_s": final["wall"] - self.t0,
            })
        sys.stdout.write(RESULT_MARKER + json.dumps(result) + "\n")
        sys.stdout.flush()


def main():
    import argparse

    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--control-port", type=int, default=8099)
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--variant", required=True)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--sink-url", required=True)
    ap.add_argument("--sample-rate", type=int, default=None)
    ap.add_argument("--upload-interval", type=int, default=None)
    ap.add_argument("--gunicorn", default="gunicorn")
    ap.add_argument("--max-run-s", type=float, default=900.0)
    args = ap.parse_args()
    Supervisor(args).run()


if __name__ == "__main__":
    main()
