"""Automatic guards.

The strongest checks are the ones where an answer is known to be impossible.
Every guard here is checked in *both* directions where a direction exists:
under-reporting is easier to miss than over-reporting precisely because it does
not look absurd.

A cell that fails a guard is marked and never ranked.
"""

from dataclasses import dataclass, field


@dataclass
class Check:
    name: str
    ok: bool
    detail: str
    scope: str = ""
    severity: str = "error"  # "error" invalidates a cell, "warn" annotates it


@dataclass
class Report:
    checks: list = field(default_factory=list)

    def add(self, name, ok, detail, scope="", severity="error"):
        self.checks.append(Check(name, ok, detail, scope, severity))
        return ok

    @property
    def failures(self):
        return [c for c in self.checks if not c.ok and c.severity == "error"]

    @property
    def warnings(self):
        return [c for c in self.checks if not c.ok and c.severity == "warn"]

    @property
    def green(self):
        return not self.failures

    def failed_scopes(self):
        return {c.scope for c in self.failures if c.scope}


# --- per-run validity --------------------------------------------------------

MIN_REGION_S = 5.0


def check_run(rep, run, sink_stats, profiles_expected, scope):
    """A run must be demonstrably valid before its numbers are used at all."""
    rid = run.get("run_id", "?")

    rep.add(
        "iterations match",
        run["iterations_completed"] == run["expected_iterations"],
        f"{rid}: completed {run['iterations_completed']}, "
        f"expected {run['expected_iterations']}",
        scope,
    )
    rep.add(
        "region long enough",
        run["wall_s"] >= MIN_REGION_S,
        f"{rid}: region lasted {run['wall_s']:.2f}s, need >= {MIN_REGION_S}s "
        "(a sub-second run reports noise that looks like a measurement)",
        scope,
    )
    rep.add(
        "cpu consumed",
        run["cpu_s"] > 0,
        f"{rid}: cgroup reported {run['cpu_s']:.3f}s of CPU",
        scope,
    )

    profiles = sink_stats.get("profiles", 0)
    if profiles_expected:
        rep.add(
            "profiler produced data",
            profiles > 0 and sink_stats.get("samples", 0) > 0,
            f"{rid}: {profiles} profiles, {sink_stats.get('samples', 0)} samples "
            "(zero means the agent never ran, and a cheap broken profiler is "
            "the default failure mode of a fast result)",
            scope,
        )
    else:
        rep.add(
            "baseline uploaded nothing",
            profiles == 0,
            f"{rid}: unprofiled variant produced {profiles} profiles",
            scope,
        )


# --- physical invariants -----------------------------------------------------

# A profile cannot contain more CPU time than the process consumed. The 5%
# allowance covers the teardown flush landing just outside the region window.
SEEN_UPPER = 1.05

# Above this fraction of wall time spent on-CPU, sample-period granularity is
# negligible and the profile's estimate must not exceed actual CPU.
BUSY_DUTY_CYCLE = 0.50

# Lower bound on how much of the accountable CPU a profile must contain. This is
# a dead-sampler detector, not a precision check: it exists because a profiler
# that has silently stopped is cheap, and under-reporting does not look absurd
# the way over-reporting does.
COLLAPSE_FLOOR = 0.40


def check_cpu_accounting(rep, shape, profiled_cpu_s, profiled_wall_s, seen_cpu_s,
                         scope, agents=1, stacks_per_tick=1):
    """Bound the profile's claimed CPU, in both directions.

    The window is the whole time the agent was running -- warmup, measured
    region and teardown -- because everything it uploaded was collected in that
    span. Bounding against the measured region alone makes a perfectly correct
    profile look like a 50% over-report.

    Two independent upper bounds, because they fail for different reasons:

    * against process CPU -- a profile cannot observe more CPU than was spent;
    * against wall time -- at the default gil_only=True the sampler records at
      most one thread per tick, so one tick's worth of nanoseconds per tick is
      the ceiling no matter how many threads are running. Two multipliers scale
      that ceiling: `agents`, for the multi-process shape where each gunicorn
      worker runs its own agent and its own GIL, and `stacks_per_tick`, which is
      the live thread count when gil_only=False because then every thread is
      unwound and recorded.

    And a lower bound, because under-reporting is the failure that gets missed:
    it does not look absurd, so nothing flags it until a workload happens to
    expose it.
    """
    if profiled_cpu_s <= 0 or profiled_wall_s <= 0:
        return

    # Duty cycle decides whether exceeding process CPU is a bug or a known
    # property of sampling.
    #
    # On a busy workload the process is nearly always on-CPU, each tick lands in
    # a slice far longer than the sample period, and the estimate converges. An
    # excess there is a real accounting bug, so it fails the run.
    #
    # On a low-duty-cycle workload the CPU arrives in slices much shorter than
    # the 10ms period, and a thread caught holding the GIL is credited a whole
    # period regardless. That over-attributes systematically -- measured at
    # ratios of 1.1 to 1.3 on the IO shapes. It is a property of the profiler
    # worth reporting, not a fault in the harness, so it is recorded as a finding
    # rather than treated as an invalid run. The physical ceiling below still
    # applies unconditionally.
    # A variant that records every thread it walks credits each one a full
    # sample period, but on a GIL-serialised workload only one of them was
    # actually on-CPU. Over-attribution is then structural, not a harness fault,
    # so it is reported rather than failing the run at any duty cycle.
    duty = profiled_cpu_s / profiled_wall_s
    busy = duty >= BUSY_DUTY_CYCLE and stacks_per_tick == 1
    rep.add(
        "seen <= cpu used",
        seen_cpu_s <= profiled_cpu_s * SEEN_UPPER,
        f"{shape}: profile claims {seen_cpu_s:.2f}s CPU, process used "
        f"{profiled_cpu_s:.2f}s while profiling (ratio "
        f"{seen_cpu_s / profiled_cpu_s:.2f}, duty cycle {duty:.2f})"
        + ("" if busy else
           "; over-attribution is expected here -- a tick credits a full sample "
           "period per stack recorded, whether that stack was on-CPU for the "
           "whole period or merely waiting for the GIL -- so it is reported "
           "rather than treated as a harness fault"),
        scope,
        severity="error" if busy else "warn",
    )
    ceiling = profiled_wall_s * agents * stacks_per_tick
    rep.add(
        "seen <= wall (gil_only ceiling)",
        seen_cpu_s <= ceiling * SEEN_UPPER,
        f"{shape}: profile claims {seen_cpu_s:.2f}s over a {profiled_wall_s:.2f}s "
        f"window with {agents} agent(s) x {stacks_per_tick} stack(s)/tick "
        f"(ratio {seen_cpu_s / ceiling:.2f}); a tick can record at most one "
        "sample period per stack it walks, so this cannot exceed 1",
        scope,
    )

    # Reference the lower bound against the *smaller* of the CPU actually spent
    # and the physical ceiling. That is the most a correct profile could be
    # expected to account for, and using it means one bound works for every
    # shape and configuration: an IO-bound workload spends little CPU, a
    # GIL-serialised one cannot be recorded faster than wall time, and a truly
    # parallel one is capped by how many stacks a tick records. A per-shape
    # table of floors was really just this quantity, guessed.
    accountable = min(profiled_cpu_s, ceiling)
    rep.add(
        "profile not collapsed",
        seen_cpu_s >= COLLAPSE_FLOOR * accountable,
        f"{shape}: profile claims {seen_cpu_s:.2f}s against {accountable:.2f}s "
        f"accountable (min of {profiled_cpu_s:.2f}s cpu spent and "
        f"{ceiling:.2f}s ceiling); floor is {COLLAPSE_FLOOR:.0%}. A sampler that "
        "has quietly stopped is cheap, and unlike over-reporting that does not "
        "look impossible, so this bound has to be explicit",
        scope,
    )


def check_upload_backpressure(rep, shape, max_post_s, upload_interval, scope):
    """A slow sink back-pressures the agent's sync_channel(10) upload queue.

    That would move profiler cost out of the CPU column and into blocked time,
    quietly shrinking the number being measured.
    """
    limit = upload_interval / 2.0
    rep.add(
        "sink kept up",
        max_post_s <= limit,
        f"{shape}: slowest upload took {max_post_s * 1000:.1f}ms, limit "
        f"{limit * 1000:.0f}ms",
        scope,
        severity="warn",
    )


# --- monotonic relationships -------------------------------------------------

def check_no_impossible_overhead(rep, cells):
    """Overhead below minus the noise floor cannot happen if the measurement is real.

    A negative value is a free signal that the whole table is unusable, not just
    that row.
    """
    for key, cell in cells.items():
        oh = cell.get("cpu_overhead_pct")
        noise = cell.get("noise_pct")
        if oh is None or oh != oh:
            continue
        rep.add(
            "overhead not impossible",
            oh >= -abs(noise if noise == noise else 0.0),
            f"{key}: cpu overhead {oh:+.1f}% against a {noise:.1f}% noise floor "
            "(a real negative invalidates the table, not just this row)",
            key,
        )


def check_depth_monotonic(rep, cells):
    """A deeper stack cannot be cheaper to unwind than a shallower one.

    Compared against `deep_stable`, not `deep`. `deep` rebuilds its stack every
    iteration, which past a datastack chunk boundary makes the sampler fail and
    drop the sample -- so `deep` really is cheaper, for a reason that has nothing
    to do with unwind cost. Checking it here would flag that as an inversion and
    bury the actual finding, which the `profile not collapsed` bound already
    reports.
    """
    deep = cells.get("deep_stable")
    shallow = cells.get("cpu_single")
    if not deep or not shallow:
        return
    d, s = deep.get("cpu_overhead_pct"), shallow.get("cpu_overhead_pct")
    n = max(deep.get("noise_pct", 0), shallow.get("noise_pct", 0))
    if d != d or s != s:
        return
    rep.add(
        "deeper stacks cost more",
        d >= s - n,
        f"deep_stable {d:+.1f}% vs cpu_single {s:+.1f}% (noise {n:.1f}%); the "
        "sampler unwinds every frame, so this cannot invert",
        "deep_stable",
        severity="warn",
    )


def check_thread_sweep(rep, sweep, walks_all_threads, variant):
    """Check the thread-count sweep against what the sampler actually does.

    py-spy applies gil_only *inside* its per-thread loop, before unwinding
    (python_spy.rs). So the expected shape of this curve depends on the config,
    and getting the direction wrong turns a correct result into a false alarm:

    * gil_only=True (the shipping default) unwinds only the GIL-holding thread.
      Every other thread costs one pointer read. Per-tick cost is therefore
      close to flat in thread count, and a curve that *rises* steeply would mean
      the pre-walk filter is not working.
    * gil_only=False unwinds every live thread, so cost must rise with thread
      count. This is the direction that proves the harness can resolve a
      thread-count signal at all -- without it, a flat curve at the default is
      indistinguishable from a harness that cannot see anything.
    """
    if len(sweep) < 2:
        return
    points = sorted(sweep.items())
    labels = [n for n, _ in points]
    values = [c.get("cpu_overhead_pct", float("nan")) for _, c in points]
    noise = max((c.get("noise_pct", 0) or 0) for _, c in points)

    lo, hi = values[0], values[-1]
    if lo != lo or hi != hi:
        return

    if walks_all_threads:
        rep.add(
            f"{variant}: overhead grows with threads",
            hi > lo + noise,
            f"{variant}: threads {labels[0]} -> {labels[-1]}: {lo:+.1f}% -> "
            f"{hi:+.1f}% (noise {noise:.1f}%); every live thread is unwound, so "
            "this must rise -- if it does not, the harness cannot resolve a "
            "thread-count signal and a flat curve elsewhere means nothing",
            "sweep",
        )
    else:
        rep.add(
            f"{variant}: overhead flat in thread count",
            hi <= lo + max(noise, 1.0) * 3.0,
            f"{variant}: threads {labels[0]} -> {labels[-1]}: {lo:+.1f}% -> "
            f"{hi:+.1f}% (noise {noise:.1f}%); gil_only skips non-GIL threads "
            "before unwinding, so cost should stay close to flat. A steep rise "
            "would mean that pre-walk filter is not taking effect",
            "sweep",
        )
