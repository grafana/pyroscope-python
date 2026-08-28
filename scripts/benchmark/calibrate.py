"""Pick iteration counts so every measured region lasts seconds, not milliseconds.

A run that finishes in tens of milliseconds collects a handful of samples and
reports overheads that are pure noise, while still looking like a working
benchmark. Calibration is what keeps the fixed-work design honest: the counts
are chosen once, against the unprofiled variant, and then held constant across
variants so that "same iterations" really is "same work".
"""

import json
import time
from pathlib import Path

import workloads

CALIBRATION_FILE = Path(__file__).with_name("calibration.json")

# Work per iteration. Fixed across shapes so a per-iteration cost difference
# between shapes reflects the shape (thread count, stack depth), not the unit.
INNER = 2000

# A churn thread should be short-lived enough to stress task-table churn, but
# not so short that thread creation is the entire workload.
CHURN_THREAD_LIFETIME_S = 0.05


def inner_for(shape):
    """Work per iteration for this shape.

    Constant across shapes by default, so a per-iteration difference reflects
    the shape's structure. The IO shapes scale it down because their iteration
    is supposed to be dominated by the block, not by the CPU slice.
    """
    return max(1, int(INNER * workloads.INNER_SCALE.get(shape, 1.0)))


def _measure_rate(shape, threads, probe_iters, server):
    fn = workloads.SHAPES[shape]
    kwargs = dict(threads=threads, batch=max(1, probe_iters), server=server)
    inner = inner_for(shape)
    t0 = time.monotonic()
    completed = fn(probe_iters, inner, **kwargs)
    elapsed = time.monotonic() - t0
    return completed, elapsed


def calibrate_shape(shape, threads, target_s, probe_s=1.0):
    """Return (iterations, batch) giving roughly `target_s` of unprofiled work."""
    server = workloads.make_server() if shape in workloads.NEEDS_SERVER else None
    try:
        # Grow a probe until it runs long enough to time reliably.
        iters = 8
        while True:
            _, elapsed = _measure_rate(shape, threads, iters, server)
            if elapsed >= probe_s or iters > 10_000_000:
                break
            scale = max(2.0, min(20.0, probe_s / max(elapsed, 1e-4)))
            iters = int(iters * scale)

        per_iter = elapsed / iters
        iterations = max(1, int(target_s / per_iter))
        batch = max(1, int(CHURN_THREAD_LIFETIME_S / per_iter))
        return iterations, batch, per_iter
    finally:
        if server is not None:
            server.close()


def run(shapes, threads, target_s, out=CALIBRATION_FILE):
    table = {}
    for shape in shapes:
        iterations, batch, per_iter = calibrate_shape(shape, threads, target_s)
        table[shape] = {
            "iterations": iterations,
            "inner": inner_for(shape),
            "batch": batch,
            "threads": threads,
            "per_iteration_s": per_iter,
            "target_s": target_s,
        }
        print(
            f"  {shape:<11} iterations={iterations:<9} inner={inner_for(shape):<5} "
            f"batch={batch:<6} ({per_iter * 1e6:.1f} us/iteration)",
            flush=True,
        )
    payload = {"threads": threads, "target_s": target_s,
               "io_wait_s": workloads.IO_WAIT_S, "shapes": table}
    Path(out).write_text(json.dumps(payload, indent=2))
    return payload


def load(out=CALIBRATION_FILE):
    p = Path(out)
    if not p.exists():
        return None
    return json.loads(p.read_text())


if __name__ == "__main__":
    import argparse

    ap = argparse.ArgumentParser()
    ap.add_argument("--target-s", type=float, default=25.0)
    ap.add_argument("--threads", type=int, default=4)
    ap.add_argument("--shapes", nargs="*", default=sorted(workloads.SHAPES))
    args = ap.parse_args()
    print(f"calibrating for ~{args.target_s}s regions on {args.threads} threads")
    run(args.shapes, args.threads, args.target_s)
