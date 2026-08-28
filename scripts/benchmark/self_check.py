#!/usr/bin/env python3
"""Validate the harness before trusting anything it reports.

Three questions, in order of how cheap they are to answer:

1. Do the guards fire? A guard that has never triggered is untested. Each one is
   fed data that must fail it, and data that must pass it.
2. What is the noise floor? Measured, not assumed, by comparing two
   byte-identical unprofiled variants. Whatever "overhead" that reports is
   noise, and no smaller difference elsewhere means anything.
3. Can the harness resolve a signal that is known to exist? That is the
   thread-count sweep (`run_bench.py --sweep`): at gil_only=False every live
   thread is unwound, so cost must rise with thread count. If it does not, a
   null result elsewhere says nothing about the profiler and everything about
   the harness.

Run this after changing the harness, and before believing a surprising result.
"""

import argparse
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import invariants
import runner
import stats


# --- 1. do the guards fire? --------------------------------------------------

def _expect(name, should_fail, fn):
    """Run one guard in isolation and assert whether it fired."""
    rep = invariants.Report()
    fn(rep)
    fired = bool(rep.failures)
    ok = fired == should_fail
    verdict = "PASS" if ok else "FAIL"
    what = "fired" if fired else "stayed quiet"
    want = "should fire" if should_fail else "should stay quiet"
    print(f"  [{verdict}] {name}: {what} ({want})")
    if not ok:
        for c in rep.checks:
            print(f"          {c.name}: ok={c.ok} :: {c.detail}")
    return ok


def guards_fire():
    """Feed each guard a case it must reject and a case it must accept."""
    print("guard tests")
    results = []

    # seen > used: a profile claiming more CPU than the process ever spent.
    results.append(_expect(
        "seen <= cpu used rejects over-reporting", True,
        lambda r: invariants.check_cpu_accounting(
            r, "cpu_single", profiled_cpu_s=10.0, profiled_wall_s=10.0,
            seen_cpu_s=15.0, scope="synthetic")))
    results.append(_expect(
        "seen <= cpu used accepts a plausible profile", False,
        lambda r: invariants.check_cpu_accounting(
            r, "cpu_single", profiled_cpu_s=10.0, profiled_wall_s=10.0,
            seen_cpu_s=9.5, scope="synthetic")))

    # Over-attribution on a low-duty-cycle workload is a measured property of
    # a 10ms sampler, not a harness fault, so it is reported rather than failing
    # the run. The ceiling below still applies.
    results.append(_expect(
        "low duty cycle over-report reported, not failed", False,
        lambda r: invariants.check_cpu_accounting(
            r, "io_bound", profiled_cpu_s=1.34, profiled_wall_s=14.0,
            seen_cpu_s=1.78, scope="synthetic")))
    results.append(_expect(
        "busy workload over-report still fails", True,
        lambda r: invariants.check_cpu_accounting(
            r, "cpu_multi", profiled_cpu_s=14.0, profiled_wall_s=14.0,
            seen_cpu_s=18.0, scope="synthetic")))

    # The bound that is easy to forget: a profile reporting almost nothing is
    # just as invalidating as one reporting too much, and looks far more
    # plausible.
    results.append(_expect(
        "lower bound rejects a collapsed profile", True,
        lambda r: invariants.check_cpu_accounting(
            r, "cpu_single", profiled_cpu_s=10.0, profiled_wall_s=10.0,
            seen_cpu_s=0.5, scope="synthetic")))
    results.append(_expect(
        "lower bound accepts an IO-bound profile", False,
        lambda r: invariants.check_cpu_accounting(
            r, "io_bound", profiled_cpu_s=2.0, profiled_wall_s=10.0,
            seen_cpu_s=1.5, scope="synthetic")))
    # A parallel workload spends more CPU than one tick can record. The bound
    # must reference what is accountable, not the raw CPU, or a correct profile
    # trips it.
    results.append(_expect(
        "lower bound accepts a GIL-capped parallel profile", False,
        lambda r: invariants.check_cpu_accounting(
            r, "cpu_multi", profiled_cpu_s=40.0, profiled_wall_s=10.0,
            seen_cpu_s=9.0, scope="synthetic")))
    results.append(_expect(
        "lower bound accepts gil_only=False on many threads", False,
        lambda r: invariants.check_cpu_accounting(
            r, "cpu_multi", profiled_cpu_s=21.9, profiled_wall_s=19.8,
            seen_cpu_s=34.3, scope="synthetic", stacks_per_tick=16)))

    # The gil_only ceiling has to scale with the number of independent agents,
    # or the multi-process shape trips a guard that is not actually violated.
    results.append(_expect(
        "ceiling scales with agent count", False,
        lambda r: invariants.check_cpu_accounting(
            r, "http", profiled_cpu_s=30.0, profiled_wall_s=10.0,
            seen_cpu_s=15.0, scope="synthetic", agents=2)))
    results.append(_expect(
        "gil ceiling still rejects the impossible", True,
        lambda r: invariants.check_cpu_accounting(
            r, "http", profiled_cpu_s=30.0, profiled_wall_s=10.0,
            seen_cpu_s=25.0, scope="synthetic", agents=2)))

    # A negative overhead beyond the noise floor cannot happen if the
    # measurement is real.
    results.append(_expect(
        "impossible negative overhead rejected", True,
        lambda r: invariants.check_no_impossible_overhead(
            r, {"cpu_single": {"cpu_overhead_pct": -26.0, "noise_pct": 1.0}})))
    results.append(_expect(
        "small negative inside the noise accepted", False,
        lambda r: invariants.check_no_impossible_overhead(
            r, {"cpu_single": {"cpu_overhead_pct": -0.4, "noise_pct": 2.0}})))

    # The sweep's expected direction depends on the config, because py-spy
    # applies gil_only before unwinding. Both directions are checked, so getting
    # the model backwards shows up here rather than as a false alarm on a real
    # run.
    flat = {1: {"cpu_overhead_pct": 3.0, "noise_pct": 0.5},
            16: {"cpu_overhead_pct": 3.1, "noise_pct": 0.5}}
    rising = {1: {"cpu_overhead_pct": 3.0, "noise_pct": 0.5},
              16: {"cpu_overhead_pct": 22.0, "noise_pct": 0.5}}
    results.append(_expect(
        "gil_only=False: flat sweep rejected", True,
        lambda r: invariants.check_thread_sweep(r, flat, True, "cpu_nogil")))
    results.append(_expect(
        "gil_only=False: rising sweep accepted", False,
        lambda r: invariants.check_thread_sweep(r, rising, True, "cpu_nogil")))
    results.append(_expect(
        "gil_only=True: flat sweep accepted", False,
        lambda r: invariants.check_thread_sweep(r, flat, False, "cpu")))
    results.append(_expect(
        "gil_only=True: steep rise rejected", True,
        lambda r: invariants.check_thread_sweep(r, rising, False, "cpu")))

    # Run validity: silently doing less work must not pass as a valid run.
    results.append(_expect(
        "short iteration count rejected", True,
        lambda r: invariants.check_run(
            r, {"run_id": "x", "iterations_completed": 900,
                "expected_iterations": 1000, "wall_s": 20.0, "cpu_s": 20.0},
            {"profiles": 1, "samples": 10}, True, "synthetic")))
    results.append(_expect(
        "millisecond run rejected", True,
        lambda r: invariants.check_run(
            r, {"run_id": "x", "iterations_completed": 1000,
                "expected_iterations": 1000, "wall_s": 0.045, "cpu_s": 0.04},
            {"profiles": 1, "samples": 4}, True, "synthetic")))
    results.append(_expect(
        "profiler that uploaded nothing rejected", True,
        lambda r: invariants.check_run(
            r, {"run_id": "x", "iterations_completed": 1000,
                "expected_iterations": 1000, "wall_s": 20.0, "cpu_s": 20.0},
            {"profiles": 0, "samples": 0}, True, "synthetic")))
    results.append(_expect(
        "baseline that uploaded something rejected", True,
        lambda r: invariants.check_run(
            r, {"run_id": "x", "iterations_completed": 1000,
                "expected_iterations": 1000, "wall_s": 20.0, "cpu_s": 20.0},
            {"profiles": 3, "samples": 99}, False, "synthetic")))
    results.append(_expect(
        "a valid run passes cleanly", False,
        lambda r: invariants.check_run(
            r, {"run_id": "x", "iterations_completed": 1000,
                "expected_iterations": 1000, "wall_s": 25.0, "cpu_s": 24.0},
            {"profiles": 3, "samples": 4000}, True, "synthetic")))

    # The noise figure itself: it must call an all-noise table unrankable.
    noise_only = stats.spread_pct([10.0, 10.4, 9.6, 10.1])
    resolvable = stats.resolvable(1.0, noise_only)
    ok = not resolvable
    print(f"  [{'PASS' if ok else 'FAIL'}] a 1.0% difference inside a "
          f"{noise_only:.1f}% spread is refused as unrankable")
    results.append(ok)

    passed = sum(results)
    print(f"  {passed}/{len(results)} guard tests passed\n")
    return all(results)


# --- 2 and 3. measured controls ----------------------------------------------

def measured_controls(args):
    """Measure the noise floor by running two identical unprofiled variants.

    Done through the ordinary matrix so it exercises the real code path rather
    than a special case. The complementary check -- that the harness can resolve
    a signal known to exist -- is the thread-count sweep (`run_bench.py
    --sweep`), where overhead must rise with thread count at gil_only=False.
    """
    argv = [
        runner.PYTHON, str(HERE / "run_bench.py"),
        "--port", str(args.port),
        "--shapes", "cpu_single", "cpu_multi",
        "--variants", "none", "none_b", "cpu",
        "--repeats", str(args.repeats),
        "--target-s", str(args.target_s),
        "--warmup-s", str(args.warmup_s),
        "--label", "self-check: noise floor control",
    ]
    print("measured controls: " + " ".join(argv[2:]))
    print("  none_b vs none -> whatever this reports is the noise floor")
    print("  cpu vs none    -> must exceed that floor to be worth reporting\n")
    return subprocess.run(argv, cwd=str(HERE)).returncode


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repeats", type=int, default=5)
    ap.add_argument("--target-s", type=float, default=20.0)
    ap.add_argument("--warmup-s", type=float, default=10.0)
    ap.add_argument("--port", type=int,
                    default=int(os.environ.get("BENCH_SINK_PORT", "4040")),
                    help="sink port; 4040 collides with a real Pyroscope server")
    ap.add_argument("--guards-only", action="store_true")
    args = ap.parse_args()

    if not guards_fire():
        print("guard tests failed; the harness is not trustworthy")
        return 1
    if args.guards_only:
        return 0
    return measured_controls(args)


if __name__ == "__main__":
    sys.exit(main())
