from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass


StopProfiler = Callable[[], None]
StartProfiler = Callable[[str, str], StopProfiler]


@dataclass(frozen=True)
class ProfilerMode:
    name: str
    start: StartProfiler


def _start_none(_application_name: str, _server_address: str) -> StopProfiler:
    # Deliberately do not import pyroscope in the baseline process.
    return lambda: None


def _start_current(application_name: str, server_address: str) -> StopProfiler:
    import pyroscope

    started = pyroscope.configure(
        application_name=application_name,
        server_address=server_address,
        cpu_enabled=True,
        mem_enabled=False,
    )
    if not started:
        raise RuntimeError("current Pyroscope CPU profiler failed to start")

    def stop() -> None:
        if not pyroscope.shutdown():
            raise RuntimeError("current Pyroscope CPU profiler failed to stop")

    return stop


# Future profilers only need another entry here; workloads and measurements stay unchanged.
MODES: dict[str, ProfilerMode] = {
    "none": ProfilerMode("none", _start_none),
    "current": ProfilerMode("current", _start_current),
}


def start_profiler(mode: str, application_name: str, server_address: str) -> StopProfiler:
    try:
        profiler = MODES[mode]
    except KeyError as error:
        choices = ", ".join(sorted(MODES))
        raise ValueError(f"unknown profiler mode {mode!r}; expected one of: {choices}") from error
    return profiler.start(application_name, server_address)

