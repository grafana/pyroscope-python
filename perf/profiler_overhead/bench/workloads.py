from __future__ import annotations

import http.client
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit


STACK_TRACE_DEPTH = 128


def run_with_stack_depth(
    depth: int, function: Callable[..., Any], *arguments: object
) -> Any:
    """Call function while retaining at least `depth` recursive wrapper frames."""
    if depth <= 0:
        return function(*arguments)
    return run_with_stack_depth(depth - 1, function, *arguments)


def exercise_stack_depth_probe(duration: float = 0.25) -> None:
    """Keep the controlled stack CPU-active long enough for sampling."""

    def probe(deadline: float) -> None:
        value = 1
        while time.perf_counter() < deadline:
            value = cpu_step(value)

    run_with_stack_depth(STACK_TRACE_DEPTH, probe, time.perf_counter() + duration)


def cpu_step(seed: int) -> int:
    """One deterministic unit of Python interpreter-heavy integer work."""
    value = seed | 1
    for index in range(1_000):
        value = ((value * 1_664_525) + 1_013_904_223 + index) & 0xFFFFFFFF
        value ^= value >> 13
    return value


@dataclass(frozen=True)
class WorkloadResult:
    operations: int
    errors: int
    elapsed_seconds: float

    @property
    def throughput(self) -> float:
        return self.operations / self.elapsed_seconds


class ThreadedWorkload:
    def __init__(self, kind: str, thread_count: int, io_url: str) -> None:
        if kind not in {"cpu", "io"}:
            raise ValueError(f"unsupported workload kind: {kind}")
        if thread_count < 1:
            raise ValueError("thread_count must be positive")

        self.kind = kind
        self.thread_count = thread_count
        self.io_url = io_url
        self._start = threading.Event()
        self._stop = threading.Event()
        self._ready = threading.Barrier(thread_count + 1)
        self._counts = [0] * thread_count
        self._errors = [0] * thread_count
        self._threads = [
            threading.Thread(target=self._worker, args=(index,), name=f"worker-{index}")
            for index in range(thread_count)
        ]

    def prepare(self) -> None:
        for thread in self._threads:
            thread.start()
        self._ready.wait(timeout=15)

    def run(self, duration: float) -> WorkloadResult:
        started = time.perf_counter()
        self._start.set()
        self._stop.wait(duration)
        self._stop.set()
        for thread in self._threads:
            thread.join(timeout=10)
            if thread.is_alive():
                raise RuntimeError(f"{thread.name} failed to stop")
        elapsed = time.perf_counter() - started
        return WorkloadResult(sum(self._counts), sum(self._errors), elapsed)

    def _worker(self, index: int) -> None:
        if self.kind == "cpu":
            self._ready.wait(timeout=15)
            self._start.wait()
            run_with_stack_depth(STACK_TRACE_DEPTH, self._run_cpu_loop, index)
            return

        connection, path = _new_connection(self.io_url)
        try:
            connection.connect()
        except OSError:
            self._errors[index] += 1
        self._ready.wait(timeout=15)
        self._start.wait()
        run_with_stack_depth(
            STACK_TRACE_DEPTH, self._run_io_loop, index, connection, path
        )

    def _run_cpu_loop(self, index: int) -> None:
        value = index + 1
        while not self._stop.is_set():
            value = cpu_step(value)
            self._counts[index] += 1

    def _run_io_loop(
        self,
        index: int,
        connection: http.client.HTTPConnection,
        path: str,
    ) -> None:
        while not self._stop.is_set():
            try:
                connection.request("GET", path)
                response = connection.getresponse()
                response.read()
                if response.status != 200:
                    self._errors[index] += 1
                else:
                    self._counts[index] += 1
            except (OSError, http.client.HTTPException):
                self._errors[index] += 1
                connection.close()
                connection, path = _new_connection(self.io_url)
        connection.close()


def _new_connection(url: str) -> tuple[http.client.HTTPConnection, str]:
    parsed = urlsplit(url)
    if parsed.scheme != "http" or not parsed.hostname:
        raise ValueError(f"I/O workload requires an http URL, got {url!r}")
    port = parsed.port or 80
    path = parsed.path or "/"
    if parsed.query:
        path = f"{path}?{parsed.query}"
    return http.client.HTTPConnection(parsed.hostname, port, timeout=5), path


def run_threaded(kind: str, thread_count: int, duration: float, io_url: str) -> WorkloadResult:
    workload = ThreadedWorkload(kind, thread_count, io_url)
    workload.prepare()
    return workload.run(duration)

