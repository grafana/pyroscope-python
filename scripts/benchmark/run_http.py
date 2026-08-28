#!/usr/bin/env python3
"""The server-grade HTTP shape: gunicorn workers under a fixed request count.

Structure of one run:

    supervisor (scope A, subject CPUs)      load generator (scope B, other CPUs)
      +- gunicorn master
         +- worker 1 (agent started in post_fork)
         +- worker 2

The server's whole process tree shares one cgroup, so cpu.stat over a fixed
number of requests is the cost of serving them -- profiler included -- with no
per-process bookkeeping. The load generator sits in a different cgroup on a
disjoint CPU set, so it neither pollutes the measurement nor competes for the
cores being measured.

The load generator must also be proven not to be the bottleneck. If it saturates,
server slowdown turns into queueing and the benchmark quietly stops measuring the
server, so its ceiling is measured against a trivial route and asserted to be
comfortably above the rate actually observed.
"""

import argparse
import json
import subprocess
import sys
import threading
import time
import urllib.request
import uuid
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import runner

# The load generator must be able to push at least this multiple of the observed
# rate, or it cannot be ruled out as the limiting factor.
LOADGEN_HEADROOM = 3.0


def _ctl(port, path, timeout=120):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=timeout) as r:
        return json.loads(r.read())


def _run_loadgen(port, path, requests, concurrency, timeout=600):
    """Load generator in its own scope on the load generator CPU set."""
    argv = [
        runner.PYTHON, str(HERE / "httpbench" / "loadgen.py"),
        "--port", str(port), "--path", path,
        "--requests", str(requests), "--concurrency", str(concurrency),
    ]
    proc = subprocess.run(
        runner.scope_cmd(runner.LOADGEN_CPUS, argv),
        capture_output=True, text=True, timeout=timeout, cwd=str(HERE),
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"loadgen exited {proc.returncode}\n{proc.stdout[-2000:]}\n{proc.stderr[-2000:]}")
    for line in reversed(proc.stdout.strip().splitlines()):
        if line.startswith("{"):
            return json.loads(line)
    raise RuntimeError(f"loadgen produced no result\n{proc.stdout[-2000:]}")


class ServerProcess:
    """The supervisor subprocess, launched inside its own measured scope."""

    def __init__(self, *, variant, run_id, sink_url, port, control_port,
                 workers, threads, sample_rate, upload_interval):
        argv = [
            runner.PYTHON, str(HERE / "httpbench" / "supervisor.py"),
            "--port", str(port), "--control-port", str(control_port),
            "--workers", str(workers), "--threads", str(threads),
            "--variant", variant, "--run-id", run_id, "--sink-url", sink_url,
            "--gunicorn", str(Path(runner.PYTHON).parent / "gunicorn"),
        ]
        if sample_rate:
            argv += ["--sample-rate", str(sample_rate)]
        if upload_interval:
            argv += ["--upload-interval", str(upload_interval)]

        self.proc = subprocess.Popen(
            runner.scope_cmd(runner.SUBJECT_CPUS, argv),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
            cwd=str(HERE), bufsize=1,
        )
        self.lines = []
        self.ready = threading.Event()
        self.result = None
        threading.Thread(target=self._pump, daemon=True).start()

    def _pump(self):
        from protocol import RESULT_MARKER
        for line in self.proc.stdout:
            self.lines.append(line.rstrip("\n"))
            if line.startswith("SUPERVISOR_READY"):
                self.ready.set()
            elif line.startswith(RESULT_MARKER):
                self.result = json.loads(line[len(RESULT_MARKER):])

    def wait_ready(self, timeout=120):
        if not self.ready.wait(timeout):
            raise RuntimeError(
                "server never signalled ready\n" + "\n".join(self.lines[-30:])
                + "\n" + (self.proc.stderr.read() if self.proc.stderr else ""))

    def finish(self, timeout=180):
        self.proc.wait(timeout=timeout)
        if self.result is None:
            raise RuntimeError(
                "server produced no result line\n" + "\n".join(self.lines[-30:]))
        return self.result


def one_run(*, variant, sink_url, requests, warmup_requests, concurrency,
            workers, threads, port, control_port, path="/work",
            sample_rate=None, upload_interval=None, measure_ceiling=False):
    run_id = f"http-{variant}-{uuid.uuid4().hex[:8]}"
    server = ServerProcess(
        variant=variant, run_id=run_id, sink_url=sink_url, port=port,
        control_port=control_port, workers=workers, threads=threads,
        sample_rate=sample_rate, upload_interval=upload_interval)
    try:
        server.wait_ready()

        # Unmeasured. Fills the sampler's process-map and per-thread caches, and
        # lets the connection pool and interpreter settle, so first-call costs
        # land here rather than in the measured region.
        _run_loadgen(port, path, warmup_requests, concurrency)

        ceiling = None
        if measure_ceiling:
            # /healthz does no work, so this measures what the load generator
            # itself can push through this connection pool.
            ceiling = _run_loadgen(port, "/healthz", warmup_requests, concurrency)

        _ctl(control_port, "/mark/start")
        load = _run_loadgen(port, path, requests, concurrency)
        _ctl(control_port, "/mark/end")
        _ctl(control_port, "/stop")
        result = server.finish()
    finally:
        if server.proc.poll() is None:
            server.proc.kill()

    result["load"] = load
    result["loadgen_ceiling"] = ceiling
    result["iterations_completed"] = load["completed"]
    result["expected_iterations"] = requests
    result["threads_configured"] = threads
    result["cpu_us_per_request"] = (
        result["cpu_s"] * 1e6 / load["completed"] if load["completed"] else float("nan"))
    return result


def calibrate_requests(sink_url, target_s, probe_requests=4000, **opts):
    """Pick a request count that makes the measured region last `target_s`.

    Same discipline as the in-process shapes: measured once against the
    unprofiled variant and then held fixed, so "same request count" means the
    same offered work in every variant. A region of a few hundred milliseconds
    would collect a handful of samples and report noise.
    """
    probe = one_run(variant="none", sink_url=sink_url, requests=probe_requests,
                    warmup_requests=max(500, probe_requests // 3),
                    measure_ceiling=False, **opts)
    rps = probe["load"]["rps"]
    requests = max(probe_requests, int(target_s * rps))
    print(f"  http        requests={requests:<9} "
          f"({rps:.0f} rps unprofiled, {probe['cpu_us_per_request']:.0f} "
          "us cpu/request)", flush=True)
    return requests, rps


def check_load(rep, result, scope):
    """Guards specific to the HTTP shape.

    Each one covers a way "N requests" can stop meaning "N requests' worth of
    server work" without the numbers looking wrong.
    """
    import invariants  # noqa: F401 - shares the Report type

    load = result["load"]
    rep.add("all requests completed",
            load["completed"] == result["expected_iterations"],
            f"{scope}: {load['completed']} of {result['expected_iterations']} completed",
            scope)
    non200 = {k: v for k, v in load["status_counts"].items() if k != "200"}
    rep.add("all responses 200", not non200,
            f"{scope}: non-200 responses {non200 or 'none'}", scope)
    rep.add("workers all forked",
            len(result["worker_pids"]) >= result["workers"],
            f"{scope}: {len(result['worker_pids'])} of {result['workers']} workers "
            "answered; a worker that never forked never ran post_fork and so was "
            "never profiled", scope)
    rep.add("gunicorn exited cleanly", result["gunicorn_returncode"] == 0,
            f"{scope}: gunicorn returncode {result['gunicorn_returncode']}", scope)

    # A long tail means requests are queueing rather than being served, which
    # breaks the fixed-work premise.
    if load["p50_ms"] > 0:
        rep.add("no queueing tail", load["p99_ms"] <= 5 * load["p50_ms"],
                f"{scope}: p99 {load['p99_ms']:.1f}ms vs p50 {load['p50_ms']:.1f}ms",
                scope, severity="warn")

    ceiling = result.get("loadgen_ceiling")
    if ceiling:
        rep.add("loadgen has headroom",
                ceiling["rps"] >= LOADGEN_HEADROOM * load["rps"],
                f"{scope}: load generator ceiling {ceiling['rps']:.0f} rps against "
                f"{load['rps']:.0f} rps observed (needs {LOADGEN_HEADROOM}x, or a "
                "saturated generator turns server slowdown into queueing)",
                scope)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--variant", required=True)
    ap.add_argument("--sink-url", default="http://127.0.0.1:4040")
    ap.add_argument("--requests", type=int, default=20000)
    ap.add_argument("--warmup-requests", type=int, default=6000)
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--control-port", type=int, default=8099)
    ap.add_argument("--path", default="/work")
    ap.add_argument("--ceiling", action="store_true")
    args = ap.parse_args()
    r = one_run(variant=args.variant, sink_url=args.sink_url,
                requests=args.requests, warmup_requests=args.warmup_requests,
                concurrency=args.concurrency, workers=args.workers,
                threads=args.threads, port=args.port,
                control_port=args.control_port, path=args.path,
                measure_ceiling=args.ceiling)
    print(json.dumps(r, indent=2))


if __name__ == "__main__":
    main()
