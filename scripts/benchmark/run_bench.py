#!/usr/bin/env python3
"""Benchmark the CPU profiler's overhead against no profiling at all.

Run this on the Linux VM (see run_linux.sh for the one-command path from macOS).

The measurement protocol, and why each part is there:

* Fixed work, not fixed duration. Overhead then has nowhere to hide.
* cgroup cpu.stat as the headline metric. The cost lands in the process's own
  CPU consumption; wall clock mostly picks up scheduler jitter.
* Repeats round-robin across the whole matrix, so drift in machine load spreads
  evenly instead of landing on whichever cell occupied a busy window.
* A noise figure beside every number, and no ranking of differences inside it.
"""

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import calibrate
import invariants
import run_http
import report as report_mod
import runner
import stats
from variants import VARIANTS, DEFAULT_VARIANTS, SWEEP_VARIANTS

DEFAULT_SHAPES = ["cpu_single", "cpu_multi", "io_bound", "mixed", "deep",
                  "deep_stable", "churn", "http"]
# Shapes driven by their own orchestrator rather than by worker.py.
EXTERNAL_SHAPES = {"http"}
SWEEP_THREADS = [1, 2, 4, 8, 16]
SHIPPING_SAMPLE_RATE = 100
SHIPPING_UPLOAD_INTERVAL = 10


# --- sink lifecycle ----------------------------------------------------------

class Sink:
    def __init__(self, port=4040):
        self.port = port
        self.url = f"http://127.0.0.1:{port}"
        self.proc = None

    def start(self):
        # Refuse to start if something already holds the port. Otherwise the
        # readiness probe below is answered by that foreign listener while our
        # own sink dies on bind, and the whole run silently depends on a process
        # the harness does not control -- which is exactly how one run got
        # killed mid-flight by an unrelated command.
        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            probe.connect(("127.0.0.1", self.port))
            raise RuntimeError(
                f"port {self.port} is already in use; stop the stale sink "
                "first (the harness will not attach to a listener it did not "
                "start)")
        except ConnectionRefusedError:
            pass
        finally:
            probe.close()

        # Log to a file rather than a pipe: nothing reads the sink's output
        # during a run, and a full pipe buffer would block the sink and stall
        # every upload behind it.
        self.log_path = Path(tempfile.gettempdir()) / "bench-sink.log"
        self.log = open(self.log_path, "w")
        self.proc = subprocess.Popen(
            ["taskset", "-c", ",".join(
                str(c) for c in runner.cpuset_list(runner.SINK_CPUS)),
             runner.PYTHON, str(HERE / "sink.py"), "--port", str(self.port)],
            cwd=str(HERE), stdout=self.log, stderr=subprocess.STDOUT,
        )
        for _ in range(50):
            if self.proc.poll() is not None:
                raise RuntimeError(
                    f"sink exited with {self.proc.returncode} during "
                    f"startup\n{self._log_tail()}")
            try:
                urllib.request.urlopen(f"{self.url}/-/ready", timeout=0.5).read()
                return
            except Exception:
                time.sleep(0.2)
        raise RuntimeError(f"sink did not become ready\n{self._log_tail()}")

    def _log_tail(self, n=40):
        try:
            return "".join(open(self.log_path).readlines()[-n:])
        except OSError:
            return "(no sink log)"

    def drain(self):
        """Fetch and clear everything the sink has accepted since the last drain.

        Decoding happens here, after the measured region, so protobuf work never
        competes with the subject or delays a POST.
        """
        # Check the sink is alive before blaming the request. A dead sink
        # otherwise surfaces as a bare closed connection several layers up, with
        # nothing to say why.
        if self.proc is not None and self.proc.poll() is not None:
            raise RuntimeError(
                f"sink exited with {self.proc.returncode}\n{self._log_tail()}")
        with urllib.request.urlopen(f"{self.url}/-/drain", timeout=300) as r:
            payload = json.loads(r.read())
        if "__sink_error__" in payload:
            raise RuntimeError("sink failed to decode uploads:\n"
                               + payload["__sink_error__"])
        return payload

    def stop(self):
        if self.proc:
            self.proc.terminate()
            self.proc.wait(timeout=10)
        if getattr(self, "log", None):
            self.log.close()


# --- one cell ----------------------------------------------------------------

def execute(sink, *, shape, variant, spec, warmup_iterations, threads,
            sample_rate, upload_interval, http_opts=None):
    sink.drain()  # discard anything left over so counts belong to this run only
    if shape == "http":
        result = run_http.one_run(
            variant=variant, sink_url=sink.url,
            sample_rate=sample_rate, upload_interval=upload_interval,
            **http_opts)
        return _attach_sink(result, sink)

    result = runner.run_worker(
        shape=shape,
        variant=variant,
        iterations=spec["iterations"],
        inner=spec["inner"],
        threads=threads,
        batch=spec["batch"],
        warmup_iterations=warmup_iterations,
        sink_url=sink.url,
        sample_rate=sample_rate,
        upload_interval=upload_interval,
    )
    return _attach_sink(result, sink)


def _attach_sink(result, sink):
    drained = sink.drain()
    result["sink"] = drained.get(result["run_id"], {
        "profiles": 0, "samples": 0, "seen_cpu_s": 0.0,
        "upload_bytes": 0, "max_post_s": 0.0, "top_functions": {},
    })
    # Uploads not tagged with this run_id would mean the partitioning is broken.
    result["sink_foreign_runs"] = [k for k in drained if k != result["run_id"]]
    return result


def build_matrix(shapes, variant_names, threads):
    cells = []
    for shape in shapes:
        for variant in variant_names:
            cells.append({"shape": shape, "variant": variant, "threads": threads,
                          "key": shape})
    return cells


def build_sweep_matrix(variant_names, thread_counts):
    cells = []
    for n in thread_counts:
        for variant in variant_names:
            cells.append({"shape": "cpu_multi", "variant": variant, "threads": n,
                          "key": f"sweep-{n}"})
    return cells


# --- aggregation -------------------------------------------------------------

def aggregate(runs, variant_names):
    """Collapse repeats into one cell per key, with the noise figure attached."""
    by_cell = defaultdict(list)
    for r in runs:
        by_cell[(r["_key"], r["variant"])].append(r)

    keys = sorted({k for k, _ in by_cell})
    out = {}
    for key in keys:
        base_runs = by_cell.get((key, "none"), [])
        if not base_runs:
            continue
        base_cpu = [r["cpu_s"] for r in base_runs]
        base_wall = [r["wall_s"] for r in base_runs]
        base_mem = [r["mem_static_bytes"] for r in base_runs]
        base_peak = [r["mem_peak_bytes"] for r in base_runs]

        entry = {
            "key": key,
            "shape": base_runs[0]["shape"],
            # Per-request cost is the natural unit for the HTTP shape: the run
            # is fixed-request-count, so this is directly comparable across
            # variants even though throughput is not.
            "none_cpu_us_per_request": stats.median(
                [r["cpu_us_per_request"] for r in base_runs]
                if "cpu_us_per_request" in base_runs[0] else []),
            "none_rps": stats.median(
                [r["load"]["rps"] for r in base_runs]
                if "load" in base_runs[0] else []),
            "threads": base_runs[0]["threads_configured"],
            "repeats": len(base_runs),
            "none": {
                "cpu_s": stats.median(base_cpu),
                "cpu_spread_pct": stats.spread_pct(base_cpu),
                "wall_s": stats.median(base_wall),
                "wall_spread_pct": stats.spread_pct(base_wall),
                "mem_static_mb": stats.median(base_mem) / 2**20,
                "mem_peak_mb": stats.median(base_peak) / 2**20,
            },
            "variants": {},
        }

        for vname in variant_names:
            if vname == "none":
                continue
            vruns = by_cell.get((key, vname), [])
            if not vruns:
                continue
            v_cpu = [r["cpu_s"] for r in vruns]
            v_wall = [r["wall_s"] for r in vruns]
            v_mem = [r["mem_static_bytes"] for r in vruns]
            v_peak = [r["mem_peak_bytes"] for r in vruns]
            seen = [r["sink"]["seen_cpu_s"] for r in vruns]
            prof_cpu = [r["profiled_cpu_s"] for r in vruns]
            prof_wall = [r["profiled_wall_s"] for r in vruns]
            samples = [r["sink"]["samples"] for r in vruns]
            profiles = [r["sink"]["profiles"] for r in vruns]
            ubytes = [r["sink"]["upload_bytes"] for r in vruns]
            maxpost = [r["sink"]["max_post_s"] for r in vruns]

            cpu_oh = stats.overhead_pct(stats.median(base_cpu), stats.median(v_cpu))
            wall_oh = stats.overhead_pct(stats.median(base_wall), stats.median(v_wall))
            noise = stats.combined_noise_pct(base_cpu, v_cpu)

            per_req = [r["cpu_us_per_request"] for r in vruns
                       if "cpu_us_per_request" in r]
            rps = [r["load"]["rps"] for r in vruns if "load" in r]
            base_per_req = [r["cpu_us_per_request"] for r in base_runs
                            if "cpu_us_per_request" in r]
            base_rps = [r["load"]["rps"] for r in base_runs if "load" in r]
            entry["variants"][vname] = {
                "repeats": len(vruns),
                "cpu_us_per_request": stats.median(per_req),
                "cpu_us_per_request_overhead_pct": (
                    stats.overhead_pct(stats.median(base_per_req),
                                       stats.median(per_req))
                    if per_req and base_per_req else float("nan")),
                "rps": stats.median(rps),
                "rps_delta_pct": (
                    stats.overhead_pct(stats.median(base_rps), stats.median(rps))
                    if rps and base_rps else float("nan")),
                "cpu_s": stats.median(v_cpu),
                "cpu_spread_pct": stats.spread_pct(v_cpu),
                "cpu_overhead_pct": cpu_oh,
                # Absolute cost, expressed as the fraction of one CPU core the
                # profiler occupies. A relative percentage against a workload
                # that barely uses the CPU can look alarming while costing very
                # little, and the reverse; both figures are needed.
                "cpu_cost_cores_pct": (
                    100.0 * (stats.median(v_cpu) - stats.median(base_cpu))
                    / stats.median(v_wall) if stats.median(v_wall) else float("nan")),
                "noise_pct": noise,
                "resolvable": stats.resolvable(cpu_oh, noise),
                "wall_s": stats.median(v_wall),
                "wall_overhead_pct": wall_oh,
                "mem_static_mb": stats.median(v_mem) / 2**20,
                "mem_delta_mb": (stats.median(v_mem) - stats.median(base_mem)) / 2**20,
                "mem_peak_mb": stats.median(v_peak) / 2**20,
                "mem_peak_delta_mb": (stats.median(v_peak) - stats.median(base_peak)) / 2**20,
                "mem_spread_pct": stats.spread_pct(v_mem),
                "seen_cpu_s": stats.median(seen),
                "seen_over_used": (stats.median(seen) / stats.median(prof_cpu)
                                   if stats.median(prof_cpu) else float("nan")),
                "seen_over_wall": (stats.median(seen) / stats.median(prof_wall)
                                   if stats.median(prof_wall) else float("nan")),
                "samples": stats.median(samples),
                "profiles": stats.median(profiles),
                "upload_bytes": stats.median(ubytes),
                "max_post_s": max(maxpost) if maxpost else 0.0,
            }
        out[key] = entry
    return out


def flatten_for_checks(agg, variant):
    return {
        key: {
            "cpu_overhead_pct": e["variants"][variant]["cpu_overhead_pct"],
            "noise_pct": e["variants"][variant]["noise_pct"],
        }
        for key, e in agg.items() if variant in e["variants"]
    }


def check_all(agg, runs, variant_names, upload_interval):
    rep = invariants.Report()

    for r in runs:
        scope = f"{r['_key']}/{r['variant']}"
        profiles_expected = VARIANTS[r["variant"]].profiles
        invariants.check_run(rep, r, r["sink"], profiles_expected, scope)
        if r["sink_foreign_runs"]:
            rep.add("sink partitioning", False,
                    f"{scope}: sink saw foreign run ids {r['sink_foreign_runs']}",
                    scope)
        if profiles_expected:
            invariants.check_cpu_accounting(
                rep, r["shape"], r["profiled_cpu_s"], r["profiled_wall_s"],
                r["sink"]["seen_cpu_s"], scope,
                agents=r.get("workers", 1),
                stacks_per_tick=(
                    r.get("worker_threads", r.get("threads_configured", 1))
                    if VARIANTS[r["variant"]].walks_all_threads else 1))
            if r["shape"] == "http":
                run_http.check_load(rep, r, scope)
            invariants.check_upload_backpressure(
                rep, r["shape"], r["sink"]["max_post_s"], upload_interval, scope)

    for vname in variant_names:
        if vname == "none":
            continue
        cells = flatten_for_checks(agg, vname)
        invariants.check_no_impossible_overhead(rep, cells)
        invariants.check_depth_monotonic(rep, cells)
    return rep


def check_sweep(agg, rep, variant_names):
    """Assert each variant's thread-count curve has the shape its config implies."""
    for vname in variant_names:
        if vname == "none":
            continue
        cells = {
            int(k.split("-")[1]): v["variants"][vname]
            for k, v in agg.items()
            if k.startswith("sweep-") and vname in v["variants"]
        }
        invariants.check_thread_sweep(
            rep, cells, VARIANTS[vname].walks_all_threads, vname)


# --- main --------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repeats", type=int, default=7)
    ap.add_argument("--target-s", type=float, default=25.0,
                    help="target duration of one measured region")
    ap.add_argument("--warmup-s", type=float, default=10.0,
                    help="unmeasured work before the region; one upload_interval "
                         "by default so the sampler's caches are populated")
    ap.add_argument("--threads", type=int, default=None,
                    help="threads for the multi-threaded shapes (default: cpuset size)")
    ap.add_argument("--shapes", nargs="*", default=DEFAULT_SHAPES)
    ap.add_argument("--variants", nargs="*", default=DEFAULT_VARIANTS)
    ap.add_argument("--sweep", action="store_true",
                    help="thread-count sweep instead of the shape matrix")
    ap.add_argument("--sample-rate", type=int, default=None,
                    help="override the shipping default; used by --self-check")
    ap.add_argument("--upload-interval", type=int, default=None)
    ap.add_argument("--quick", action="store_true",
                    help="3 repeats and 8s regions, for iterating on the harness")
    ap.add_argument("--recalibrate", action="store_true")
    # 4040 is Pyroscope's own default port, so it is frequently taken on a host
    # that runs Pyroscope. BENCH_SINK_PORT moves the sink aside; the startup
    # guard refuses to attach to a listener the harness did not start, rather
    # than silently uploading the benchmark into someone's real server.
    ap.add_argument("--port", type=int,
                    default=int(os.environ.get("BENCH_SINK_PORT", "4040")))
    ap.add_argument("--http-requests", type=int, default=20000,
                    help="measured requests for the http shape")
    ap.add_argument("--http-warmup-requests", type=int, default=6000)
    ap.add_argument("--http-concurrency", type=int, default=8)
    ap.add_argument("--http-workers", type=int, default=2)
    ap.add_argument("--http-worker-threads", type=int, default=4)
    ap.add_argument("--http-port", type=int,
                    default=int(os.environ.get("BENCH_HTTP_PORT", "8080")))
    ap.add_argument("--http-control-port", type=int,
                    default=int(os.environ.get("BENCH_HTTP_CONTROL_PORT", "8099")))
    ap.add_argument("--out-dir", default=None)
    ap.add_argument("--label", default="")
    args = ap.parse_args()

    if args.quick:
        args.repeats = min(args.repeats, 3)
        args.target_s = min(args.target_s, 8.0)
        args.warmup_s = min(args.warmup_s, 4.0)
        args.http_requests = min(args.http_requests, 4000)
        args.http_warmup_requests = min(args.http_warmup_requests, 1500)

    threads = args.threads or runner.cpuset_size(runner.SUBJECT_CPUS)
    shapes = list(args.shapes)
    if args.sweep:
        shapes = ["cpu_multi"]
        if args.variants == DEFAULT_VARIANTS:
            args.variants = list(SWEEP_VARIANTS)

    # Calibration is done unprofiled and then held fixed, so "same iterations"
    # really is "same work" for every variant.
    cal = calibrate.load()
    need = args.recalibrate or cal is None or cal["target_s"] != args.target_s \
        or cal["threads"] != threads \
        or any(s not in cal["shapes"] for s in set(shapes) - EXTERNAL_SHAPES)
    if need:
        print(f"calibrating for ~{args.target_s:.0f}s regions, {threads} threads")
        cal = calibrate.run(
            sorted((set(shapes) | set(DEFAULT_SHAPES)) - EXTERNAL_SHAPES),
            threads, args.target_s)

    matrix = (build_sweep_matrix(args.variants, SWEEP_THREADS) if args.sweep
              else build_matrix(shapes, args.variants, threads))

    http_opts = dict(
        requests=args.http_requests,
        warmup_requests=args.http_warmup_requests,
        concurrency=args.http_concurrency,
        workers=args.http_workers,
        threads=args.http_worker_threads,
        port=args.http_port,
        control_port=args.http_control_port,
        # Measured once per run: the ceiling has to be re-established whenever
        # the machine's state could have changed.
        measure_ceiling=True,
    )

    sink = Sink(args.port)
    sink.start()

    if "http" in shapes:
        # Calibrated against the running sink so the probe exercises the same
        # path as the measured runs.
        http_probe_opts = {k: v for k, v in http_opts.items()
                           if k not in ("requests", "warmup_requests",
                                        "measure_ceiling")}
        requests, probe_rps = run_http.calibrate_requests(
            sink.url, args.target_s,
            probe_requests=args.http_requests, **http_probe_opts)
        http_opts["requests"] = requests
        http_opts["warmup_requests"] = max(args.http_warmup_requests,
                                           requests // 4)

    runs = []
    total = args.repeats * len(matrix)
    n = 0
    started = time.monotonic()
    try:
        # Round-robin over the whole matrix. Batching all repeats of one cell
        # would make the comparison hostage to drift in machine load: whichever
        # cell occupied a busy window looks slower, and that bias can exceed the
        # effect being measured.
        for repeat in range(args.repeats):
            for cell in matrix:
                n += 1
                spec = dict(cal["shapes"].get(cell["shape"], {}))
                if args.sweep:
                    # Hold *total* work constant across the sweep and spread it
                    # over more threads, so thread count is the only thing that
                    # changes. Holding per-thread work fixed instead would scale
                    # total work with N: the 1-thread region would be too short
                    # to measure and the 16-thread one would take sixteen times
                    # as long, and the overhead percentage would no longer
                    # isolate thread-count sensitivity.
                    total_iterations = spec["iterations"] * cal["threads"]
                    spec["iterations"] = max(
                        1, total_iterations // cell["threads"])
                label = f"{cell['key']}/{cell['variant']}"
                elapsed = time.monotonic() - started
                eta = (elapsed / max(n - 1, 1)) * (total - n + 1)
                print(f"[{n}/{total}] repeat {repeat + 1} {label:<28} "
                      f"eta {eta / 60:.1f}m", flush=True)
                r = execute(
                    sink,
                    shape=cell["shape"], variant=cell["variant"], spec=spec,
                    warmup_iterations=max(
                        1, int(spec.get("iterations", 1)
                               * args.warmup_s / args.target_s)),
                    threads=cell["threads"],
                    sample_rate=args.sample_rate,
                    upload_interval=args.upload_interval,
                    http_opts=http_opts,
                )
                r["_key"] = cell["key"]
                r["_repeat"] = repeat
                runs.append(r)
    finally:
        sink.stop()

    agg = aggregate(runs, args.variants)
    effective_rate = args.sample_rate or SHIPPING_SAMPLE_RATE
    effective_interval = args.upload_interval or SHIPPING_UPLOAD_INTERVAL
    rep = check_all(agg, runs, args.variants, effective_interval)

    if args.sweep:
        check_sweep(agg, rep, args.variants)

    stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    out_dir = Path(args.out_dir or (HERE / "results" / stamp))
    out_dir.mkdir(parents=True, exist_ok=True)

    meta = {
        "timestamp_utc": datetime.now(timezone.utc).isoformat(),
        "label": args.label,
        "mode": "sweep" if args.sweep else "matrix",
        "repeats": args.repeats,
        "target_region_s": args.target_s,
        "warmup_s": args.warmup_s,
        "threads": threads,
        "shapes": shapes,
        "http": http_opts,
        "variants": args.variants,
        # Carried into the report so a result is never read without the note on
        # what a tick of that variant actually costs.
        "variant_detail": {
            v: {"label": VARIANTS[v].label, "per_tick": VARIANTS[v].per_tick,
                "walks_all_threads": VARIANTS[v].walks_all_threads,
                "config": VARIANTS[v].config_note()}
            for v in args.variants
        },
        "sample_rate": effective_rate,
        "upload_interval": effective_interval,
        "sample_rate_is_default": args.sample_rate is None,
        "subject_cpus": runner.SUBJECT_CPUS,
        "sink_cpus": runner.SINK_CPUS,
        "loadgen_cpus": runner.LOADGEN_CPUS,
        "uname": subprocess.run(["uname", "-srm"], capture_output=True,
                                text=True).stdout.strip(),
        "python": sys.version.split()[0],
        "calibration": cal,
    }

    (out_dir / "results.json").write_text(json.dumps(
        {"meta": meta, "aggregate": agg, "runs": runs,
         "checks": [c.__dict__ for c in rep.checks]}, indent=2, default=str))
    (out_dir / "report.md").write_text(report_mod.markdown(meta, agg, rep))
    (out_dir / "report.html").write_text(report_mod.html(meta, agg, rep))

    print()
    print(report_mod.markdown(meta, agg, rep))
    print(f"\nwritten to {out_dir}")
    return 0 if rep.green else 1


if __name__ == "__main__":
    sys.exit(main())
