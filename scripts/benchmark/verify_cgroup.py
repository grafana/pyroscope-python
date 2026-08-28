#!/usr/bin/env python3
"""Check that the measurement instrument itself is accurate.

Everything the benchmark reports rests on cgroup cpu.stat being a faithful
account of the CPU a run consumed, and on the cpuset pinning actually applying.
Both are checked here against work whose cost is known independently, so a
plumbing fault surfaces as a failure here rather than as a plausible-looking
overhead number later.

Run inside a scope:
    sudo systemd-run --scope -q -p AllowedCPUs=2-5 -p CPUAccounting=yes \
        -p MemoryAccounting=yes -- python verify_cgroup.py --expect-cpus 4
"""

import argparse
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from cgroup import CgroupCounters, nproc_effective

TOLERANCE = 0.05


def _spin(deadline):
    while time.monotonic() < deadline:
        for _ in range(2000):
            pass


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--expect-cpus", type=int, required=True)
    ap.add_argument("--seconds", type=float, default=6.0)
    ap.add_argument("--threads", type=int, default=2)
    args = ap.parse_args()

    failures = []

    # 1. Pinning applied. If the cpuset did not take, every duration below is
    # measured on the wrong number of cores.
    cpus = nproc_effective()
    ok = cpus == args.expect_cpus
    print(f"[{'PASS' if ok else 'FAIL'}] cpuset applied: {cpus} cpus visible, "
          f"expected {args.expect_cpus}")
    if not ok:
        failures.append("cpuset")

    c = CgroupCounters()
    print(f"       cgroup: {c.path}")

    # 2. N threads spinning for T seconds must consume N*T CPU seconds, because
    # busy-looping in Python holds the GIL and only one thread runs at a time.
    # So the expectation is T, not N*T -- and that is itself worth asserting,
    # since a wrong answer here would mean the counter is summing something else.
    t0 = c.cpu_usec()
    deadline = time.monotonic() + args.seconds
    threads = [threading.Thread(target=_spin, args=(deadline,))
               for _ in range(args.threads)]
    wall0 = time.monotonic()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.monotonic() - wall0
    cpu = (c.cpu_usec() - t0) / 1e6

    # GIL-bound: total CPU should track wall time, within scheduling slack.
    ratio = cpu / wall
    ok = abs(ratio - 1.0) <= 0.15
    print(f"[{'PASS' if ok else 'FAIL'}] {args.threads} GIL-bound threads for "
          f"{wall:.2f}s wall consumed {cpu:.2f}s cgroup cpu (ratio {ratio:.3f}, "
          "expected ~1.0 since the GIL serialises them)")
    if not ok:
        failures.append("gil-bound cpu accounting")

    # 3. True parallelism: N threads each sleeping consume ~no CPU, so the
    # counter must not simply track wall time.
    t0 = c.cpu_usec()
    wall0 = time.monotonic()
    time.sleep(2.0)
    idle_cpu = (c.cpu_usec() - t0) / 1e6
    idle_wall = time.monotonic() - wall0
    ok = idle_cpu < 0.10 * idle_wall
    print(f"[{'PASS' if ok else 'FAIL'}] {idle_wall:.2f}s idle consumed "
          f"{idle_cpu:.3f}s cpu (must be near zero, or the counter is tracking "
          "wall time rather than cpu)")
    if not ok:
        failures.append("idle cpu accounting")

    # 4. Memory counters move in the right direction and are readable.
    before = c.memory_current()
    ballast = bytearray(64 * 1024 * 1024)
    ballast[::4096] = b"\x01" * (len(ballast) // 4096)
    after = c.memory_current()
    peak = c.memory_peak()
    grew = (after - before) / 2**20
    ok = grew > 32 and peak >= after
    print(f"[{'PASS' if ok else 'FAIL'}] allocating 64 MiB moved "
          f"memory.current by {grew:.1f} MiB, memory.peak "
          f"{peak / 2**20:.1f} MiB >= current {after / 2**20:.1f} MiB")
    if not ok:
        failures.append("memory accounting")
    del ballast

    print()
    if failures:
        print("FAILED: " + ", ".join(failures))
        return 1
    print("cgroup plumbing verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
