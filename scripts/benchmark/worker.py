"""One measured run, in its own process inside its own cgroup scope.

Sequencing matters more than anything else in this file:

  import -> variant.start() -> WARM UP -> read start counters
         -> exactly N iterations -> read end counters
         -> variant.stop() -> read final counters -> emit JSON

The warmup exists because py-spy scans /proc/<pid>/maps and builds a per-thread
frame cache on its first samples; that one-off cost must not land in the
measured region. The counters are read *after* stop() as well, because
pyroscope.shutdown() joins the sampler, snapshot and upload threads and
completes the final POST before returning -- so the region-plus-teardown figure
is the one that includes the profiler's full cost.
"""

import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import workloads
from protocol import RESULT_MARKER
from cgroup import CgroupCounters, nproc_effective
from variants import VARIANTS, RunConfig


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shape", required=True, choices=sorted(workloads.SHAPES))
    ap.add_argument("--variant", required=True, choices=sorted(VARIANTS))
    ap.add_argument("--iterations", type=int, required=True)
    ap.add_argument("--inner", type=int, required=True)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--batch", type=int, default=8, help="churn: units per throwaway thread")
    ap.add_argument("--warmup-iterations", type=int, required=True,
                    help="unmeasured iterations before the region, scaled from "
                         "the calibrated count")
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--sink-url", required=True)
    ap.add_argument("--sample-rate", type=int, default=None)
    ap.add_argument("--upload-interval", type=int, default=None)
    args = ap.parse_args()

    # Imported for every variant, including `none`, so the two differ only in
    # whether the agent is started.
    import pyroscope  # noqa: F401

    counters = CgroupCounters()
    variant = VARIANTS[args.variant]
    cfg = RunConfig(
        run_id=args.run_id,
        sink_url=args.sink_url,
        sample_rate=args.sample_rate,
        upload_interval=args.upload_interval,
    )

    shape_fn = workloads.SHAPES[args.shape]
    server = workloads.make_server() if args.shape in workloads.NEEDS_SERVER else None
    kwargs = dict(threads=args.threads, batch=args.batch, server=server)

    result = {
        "shape": args.shape,
        "variant": args.variant,
        "run_id": args.run_id,
        "iterations_configured": args.iterations,
        "inner": args.inner,
        "threads_configured": args.threads,
        "worker_threads": workloads.worker_threads(args.shape, args.threads),
        "cpus": nproc_effective(),
        "sample_rate": args.sample_rate,
        "upload_interval": args.upload_interval,
        "pid": os.getpid(),
    }

    if variant.start is not None:
        variant.start(cfg)
    # Opening edge of the window the agent is actually observing. Everything it
    # uploads was collected between here and stop(), so this is what its claimed
    # CPU has to be bounded against.
    profile_start = counters.snapshot()
    profile_wall_t0 = time.monotonic()

    # --- warmup: unmeasured, but the same work the region will do, in one
    # pass. Splitting it into many short passes would churn threads and
    # connections in a way the measured region does not, so the caches it warms
    # would not be the caches the region uses.
    warm_t0 = time.monotonic()
    result["warmup_iterations"] = shape_fn(args.warmup_iterations, args.inner,
                                           **kwargs)
    result["warmup_wall_s"] = time.monotonic() - warm_t0
    result["warmup_cpu_s"] = counters.cpu_usec() / 1e6

    # --- measured region
    start = counters.snapshot()
    t0 = time.monotonic()
    completed = shape_fn(args.iterations, args.inner, **kwargs)
    wall = time.monotonic() - t0
    end = counters.snapshot()

    if variant.stop is not None:
        variant.stop()
    profile_wall = time.monotonic() - profile_wall_t0

    if server is not None:
        server.close()

    final = counters.snapshot()

    result.update({
        "iterations_completed": completed,
        "wall_s": wall,
        "cpu_s": (end["usage_usec"] - start["usage_usec"]) / 1e6,
        "cpu_user_s": (end["user_usec"] - start["user_usec"]) / 1e6,
        "cpu_system_s": (end["system_usec"] - start["system_usec"]) / 1e6,
        # Includes the teardown: final flush, encode and upload.
        "cpu_with_teardown_s": (final["usage_usec"] - start["usage_usec"]) / 1e6,
        # The full window the agent observed: warmup, region and teardown.
        "profiled_cpu_s": (final["usage_usec"] - profile_start["usage_usec"]) / 1e6,
        "profiled_wall_s": profile_wall,
        # Steady-state footprint, sampled after warmup so the profiler's caches
        # are populated, but before the workload reaches its own peak.
        "mem_static_bytes": start["memory_current"],
        "mem_end_bytes": end["memory_current"],
        "mem_peak_bytes": final["memory_peak"],
    })

    # Handed back on stdout rather than via a file: the scope runs as root while
    # the orchestrator runs as the user, and fs.protected_regular blocks root
    # from writing a user-owned file in a sticky world-writable directory.
    sys.stdout.write(RESULT_MARKER + json.dumps(result) + "\n")


if __name__ == "__main__":
    main()
