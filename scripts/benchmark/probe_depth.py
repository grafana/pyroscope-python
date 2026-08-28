#!/usr/bin/env python3
"""Measure how much of a profile survives as stack depth grows.

Exists because the `deep` shape produced a *cheaper* result than the shallow one,
which is the signature of a subject that has stopped doing its job rather than
one that got faster. Run inside a scope, one depth per invocation:

    sudo systemd-run --scope -q -p AllowedCPUs=2-5 -p CPUAccounting=yes \\
        -p MemoryAccounting=yes -- python probe_depth.py --depth 256

Then read the summed sample values back from the sink and compare against the
CPU the process actually spent. A ratio that falls with depth while CPU stays
flat means the sampler is failing to unwind and the failures are being
discarded, not that it is working harder.

Note for whoever chases this further: py-spy reports a failed unwind on
`Sample.sampling_errors`, and `rust/src/pyspy_backend.rs` iterates only
`sample.traces`. A sample that failed therefore contributes nothing and logs
nothing.
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--depth", type=int, required=True)
    ap.add_argument("--seconds", type=float, default=8.0)
    ap.add_argument("--inner", type=int, default=2000)
    ap.add_argument("--threads", type=int, default=1)
    ap.add_argument("--sink-url", default="http://127.0.0.1:"
                    + os.environ.get("BENCH_SINK_PORT", "4040"))
    ap.add_argument("--gil-only", action="store_true",
                    help="default is gil_only=False, matching the benchmark")
    args = ap.parse_args()

    import pyroscope

    counters = CgroupCounters()
    run_id = f"depth-{args.depth:04d}"
    pyroscope.configure(
        application_name="depth-probe",
        server_address=args.sink_url,
        tags={"run_id": run_id},
        gil_only=args.gil_only,
        upload_interval=3,
    )

    cpu0 = counters.cpu_usec()
    t0 = time.monotonic()
    stop = threading.Event()

    def work():
        while not stop.is_set():
            workloads._nest(args.depth, workloads._burn, args.inner)

    threads = [threading.Thread(target=work) for _ in range(args.threads)]
    for t in threads:
        t.start()
    time.sleep(args.seconds)
    stop.set()
    for t in threads:
        t.join()

    wall = time.monotonic() - t0
    pyroscope.shutdown()
    cpu = (counters.cpu_usec() - cpu0) / 1e6

    sys.stdout.write(RESULT_MARKER + json.dumps({
        "run_id": run_id, "depth": args.depth, "threads": args.threads,
        "wall_s": wall, "cpu_s": cpu, "gil_only": args.gil_only,
    }) + "\n")


if __name__ == "__main__":
    main()
