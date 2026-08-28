"""Profiler variants under comparison.

This is the extension point. Adding the experimental profiler later is one entry
in VARIANTS -- everything else in the harness is variant-agnostic.

The comparison is only meaningful if the profiler is the sole variable, so every
variant runs the identical code path: same imports (including `import
pyroscope`), same warmup, same subprocess launch, same local sink. The only
difference is whether `start` is called, and with what. Do not add a fast path
for `none`.

Each variant carries its exact configure() arguments so the report can state the
configuration it measured rather than asserting one. Getting that wrong is easy
and invisible: an earlier version of this file printed "gil_only=True" in every
report header regardless of what was actually run.
"""

from dataclasses import dataclass, field
from types import MappingProxyType
from typing import Callable, Mapping, Optional


@dataclass(frozen=True)
class RunConfig:
    run_id: str
    sink_url: str
    app_name: str = "bench"
    # Manual overrides, via run_bench.py --sample-rate / --upload-interval.
    # Nothing sets these automatically: the benchmark measures the shipping
    # 100 Hz only.
    sample_rate: Optional[int] = None
    upload_interval: Optional[int] = None


@dataclass(frozen=True)
class Variant:
    name: str
    label: str
    start: Optional[Callable[[RunConfig], object]]
    stop: Optional[Callable[[], object]]
    # What one tick of this variant actually buys, for the report. "Equal
    # configuration" is rarely "equal work".
    per_tick: str = ""
    # Whether a tick unwinds every live Python thread, or only one. This decides
    # what the thread-count sweep must show and how high the profile's claimed
    # CPU may legitimately reach, so it is declared per variant rather than
    # assumed by the checks.
    walks_all_threads: bool = False
    # Non-default configure() arguments, for the report.
    profiler_kwargs: Mapping = field(default_factory=lambda: MappingProxyType({}))

    @property
    def profiles(self):
        return self.start is not None

    def config_note(self):
        if not self.profiles:
            return "no agent started"
        shown = shipping_defaults()
        shown.update(self.profiler_kwargs)
        return ", ".join(f"{k}={v}" for k, v in sorted(shown.items()))


# Settings worth restating in every report, so an override is visible by
# contrast rather than by omission.
#
# The values are read from pyroscope.configure()'s own signature rather than
# copied here. A copied number would be printed as fact while the run actually
# used whatever upstream changed it to -- and nothing would flag the mismatch.
# The fallback covers running the guard tests somewhere the package is not
# installed; it is only ever used for display.
REPORTED_SETTINGS = ("sample_rate", "oncpu", "gil_only", "upload_interval",
                     "mem_enabled")

_FALLBACK_DEFAULTS = {
    "sample_rate": 100,
    "oncpu": True,
    "gil_only": True,
    "upload_interval": 10,
    "mem_enabled": False,
}


def shipping_defaults():
    """The defaults pyroscope.configure() actually applies."""
    try:
        import inspect

        import pyroscope

        params = inspect.signature(pyroscope.configure).parameters
        return {
            k: params[k].default
            for k in REPORTED_SETTINGS
            if k in params and params[k].default is not inspect.Parameter.empty
        }
    except Exception:  # noqa: BLE001 - display only, never fail a run for this
        return dict(_FALLBACK_DEFAULTS)


def _make_start(**profiler_kwargs):
    """Build a start function applying `profiler_kwargs` over the defaults."""

    def start(cfg: RunConfig):
        import pyroscope

        kwargs = dict(
            application_name=cfg.app_name,
            server_address=cfg.sink_url,
            tags={"run_id": cfg.run_id},
        )
        kwargs.update(profiler_kwargs)
        # Run-level overrides win, so a rate can be varied from the command line
        # without defining a variant per rate.
        if cfg.sample_rate is not None:
            kwargs["sample_rate"] = cfg.sample_rate
        if cfg.upload_interval is not None:
            kwargs["upload_interval"] = cfg.upload_interval

        if not pyroscope.configure(**kwargs):
            raise RuntimeError("pyroscope.configure() returned False")
        return True

    return start


def _stop_cpu():
    import pyroscope

    # Joins the sampler thread, the snapshot thread and the upload thread, and
    # completes the final POST before returning. Counters must therefore be read
    # after this call to capture the profiler's full cost.
    if not pyroscope.shutdown():
        raise RuntimeError("pyroscope.shutdown() returned False")
    return True


VARIANTS = {
    "none": Variant(
        name="none",
        label="no profiling",
        start=None,
        stop=None,
        per_tick="n/a",
    ),
    # The measured configuration. gil_only=False makes py-spy unwind every live
    # Python thread per tick instead of only the GIL holder, so per-tick cost is
    # proportional to thread count and stack depth.
    "cpu": Variant(
        name="cpu",
        label="cpu profiler (py-spy), gil_only=False",
        start=_make_start(gil_only=False),
        stop=_stop_cpu,
        per_tick="one stack unwound per live Python thread; oncpu filters after "
                 "the unwind, so a discarded trace has already been paid for",
        walks_all_threads=True,
        profiler_kwargs=MappingProxyType({"gil_only": False}),
    ),
    # The shipping default, kept available for comparison. py-spy applies
    # gil_only inside its per-thread loop *before* get_stack_trace
    # (python_spy.rs), so here a tick unwinds exactly one stack no matter how
    # many threads are live.
    "cpu_gil_only": Variant(
        name="cpu_gil_only",
        label="cpu profiler (py-spy), shipping defaults",
        start=_make_start(),
        stop=_stop_cpu,
        per_tick="one stack unwound (the GIL holder) plus one pointer read per "
                 "other thread",
        walks_all_threads=False,
    ),
}

# Self-check-only control. Not part of any reported comparison; it exists so the
# harness can be tested against an answer that is already known.
#
#   none_b -- byte-identical to `none`. Any "overhead" it reports is pure noise,
#             which is how the noise floor gets measured rather than guessed.
#
# The other half of a self-check is a signal the harness must be able to
# resolve, since a null result from a harness that cannot see anything means
# nothing. That role is filled by the thread-count sweep, which has the
# advantage of running at the shipping sample rate rather than an amplified one.
VARIANTS["none_b"] = Variant(
    name="none_b",
    label="no profiling (noise-floor control)",
    start=None,
    stop=None,
    per_tick="n/a",
)
DEFAULT_VARIANTS = ["none", "cpu"]
SELF_CHECK_VARIANTS = ["none", "none_b", "cpu"]
SWEEP_VARIANTS = ["none", "cpu"]
