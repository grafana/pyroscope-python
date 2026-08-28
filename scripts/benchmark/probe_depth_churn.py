#!/usr/bin/env python3
"""Discriminate "deep stacks are hard to unwind" from "growing and shrinking a
deep stack is hard to unwind".

Both modes hold the same nesting depth. They differ only in whether the frame
stack is repeatedly built and torn down:

  churn   -- recurse to depth, return, repeat. The Python frame stack grows and
             shrinks continuously. Past roughly 130-150 frames of this shape it
             spans more than one CPython datastack chunk (16 KiB each), so the
             interpreter allocates and releases chunks on every pass.
  stable  -- recurse to depth once, then do all the work at the bottom without
             returning. Same depth, same per-tick unwind length, no chunk churn.

If sample loss tracks depth alone, both modes lose samples equally. If it tracks
chunk churn, `stable` keeps its samples and `churn` does not. The sampler reads
the frame chain while the process runs, and a pointer read that lands in a chunk
being recycled fails -- which aborts the whole sample, because
get_stack_trace propagates that error rather than skipping the frame.

Run one invocation per (mode, depth) inside a scope, then read the summed sample
values back from the sink.
"""

import argparse
import json
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import workloads
from cgroup import CgroupCounters
from protocol import RESULT_MARKER


def _burn_until(stop, inner):
    """Work at the bottom of the stack, never returning until told."""
    while not stop.is_set():
        workloads._burn(inner)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--depth", type=int, required=True)
    ap.add_argument("--mode", choices=["churn", "stable"], required=True)
    ap.add_argument("--seconds", type=float, default=8.0)
    ap.add_argument("--inner", type=int, default=2000)
    ap.add_argument("--sink-url", default="http://127.0.0.1:"
                    + os.environ.get("BENCH_SINK_PORT", "4040"))
    ap.add_argument("--profile", action="store_true",
                    help="omit to measure the unprofiled baseline")
    args = ap.parse_args()

    counters = CgroupCounters()
    run_id = f"{args.mode}-{args.depth:04d}-{'cpu' if args.profile else 'none'}"

    if args.profile:
        import pyroscope
        pyroscope.configure(
            application_name="depth-churn",
            server_address=args.sink_url,
            tags={"run_id": run_id},
            gil_only=False,
            upload_interval=3,
        )

    stop = threading.Event()

    def churn():
        while not stop.is_set():
            workloads._nest(args.depth, workloads._burn, args.inner)

    def stable():
        # One descent, then all the work happens at the bottom.
        workloads._nest(args.depth, _burn_until, stop, args.inner)

    cpu0 = counters.cpu_usec()
    t0 = time.monotonic()
    t = threading.Thread(target=churn if args.mode == "churn" else stable)
    t.start()
    time.sleep(args.seconds)
    stop.set()
    t.join()
    wall = time.monotonic() - t0

    if args.profile:
        import pyroscope
        pyroscope.shutdown()
    cpu = (counters.cpu_usec() - cpu0) / 1e6

    sys.stdout.write(RESULT_MARKER + json.dumps({
        "run_id": run_id, "mode": args.mode, "depth": args.depth,
        "profiled": args.profile, "wall_s": wall, "cpu_s": cpu,
    }) + "\n")


if __name__ == "__main__":
    main()
