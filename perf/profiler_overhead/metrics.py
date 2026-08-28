from __future__ import annotations

import statistics
import threading
import time
from dataclasses import dataclass
from pathlib import Path


def parse_key_values(text: str) -> dict[str, int]:
    values: dict[str, int] = {}
    for line in text.splitlines():
        key, value = line.split(maxsplit=1)
        values[key] = int(value)
    return values


def locate_cgroup(pid: int, root: Path = Path("/sys/fs/cgroup")) -> Path:
    for line in Path(f"/proc/{pid}/cgroup").read_text().splitlines():
        hierarchy, controllers, relative = line.split(":", 2)
        if hierarchy == "0" and controllers == "":
            root = root.resolve()
            path = (root / relative.lstrip("/")).resolve()
            if path != root and root not in path.parents:
                raise RuntimeError(f"unsafe cgroup path for pid {pid}: {relative}")
            return path
    raise RuntimeError(f"pid {pid} is not in a cgroup v2 hierarchy")


@dataclass(frozen=True)
class Snapshot:
    cpu_usage_usec: int
    cpu_user_usec: int
    cpu_system_usec: int
    memory_current: int
    memory_anon: int
    pids_current: int


def read_snapshot(path: Path) -> Snapshot:
    cpu = parse_key_values((path / "cpu.stat").read_text())
    memory = parse_key_values((path / "memory.stat").read_text())
    return Snapshot(
        cpu_usage_usec=cpu["usage_usec"],
        cpu_user_usec=cpu["user_usec"],
        cpu_system_usec=cpu["system_usec"],
        memory_current=int((path / "memory.current").read_text()),
        memory_anon=memory["anon"],
        pids_current=int((path / "pids.current").read_text()),
    )


class CgroupMonitor:
    def __init__(self, path: Path, interval: float = 0.05) -> None:
        self.path = path
        self.interval = interval
        self._stop = threading.Event()
        self._samples: list[Snapshot] = []
        self._error: BaseException | None = None
        self._thread = threading.Thread(target=self._sample, name="cgroup-monitor", daemon=True)
        self._started_at = 0.0

    def start(self) -> None:
        self._started_at = time.monotonic()
        self._samples.append(read_snapshot(self.path))
        self._thread.start()

    def _sample(self) -> None:
        while not self._stop.wait(self.interval):
            try:
                self._samples.append(read_snapshot(self.path))
            except BaseException as error:
                self._error = error
                return

    def finish(self, cpu_count: int) -> dict[str, float | int]:
        self._stop.set()
        self._thread.join(timeout=2)
        self._samples.append(read_snapshot(self.path))
        if self._error is not None:
            raise RuntimeError("cgroup monitor failed") from self._error

        first = self._samples[0]
        last = self._samples[-1]
        elapsed = time.monotonic() - self._started_at
        cpu_seconds = (last.cpu_usage_usec - first.cpu_usage_usec) / 1_000_000
        memory_values = [sample.memory_current for sample in self._samples]
        anon_values = [sample.memory_anon for sample in self._samples]
        return {
            "measurement_wall_seconds": elapsed,
            "cpu_seconds": cpu_seconds,
            "cpu_user_seconds": (last.cpu_user_usec - first.cpu_user_usec) / 1_000_000,
            "cpu_system_seconds": (
                last.cpu_system_usec - first.cpu_system_usec
            ) / 1_000_000,
            "cpu_utilization_percent": 100 * cpu_seconds / elapsed / cpu_count,
            "memory_start_bytes": first.memory_current,
            "memory_end_bytes": last.memory_current,
            "memory_mean_bytes": statistics.fmean(memory_values),
            "memory_peak_bytes": max(memory_values),
            "anon_memory_mean_bytes": statistics.fmean(anon_values),
            "anon_memory_peak_bytes": max(anon_values),
            "pids_peak": max(sample.pids_current for sample in self._samples),
            "memory_samples": len(self._samples),
        }

