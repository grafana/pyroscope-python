from __future__ import annotations

import argparse
import http.client
import json
import os
import threading
import time
from collections.abc import Callable
from pathlib import Path
from urllib.parse import urlsplit


def percentile(sorted_values: list[float], quantile: float) -> float:
    if not sorted_values:
        return 0.0
    position = (len(sorted_values) - 1) * quantile
    lower = int(position)
    upper = min(lower + 1, len(sorted_values) - 1)
    fraction = position - lower
    return sorted_values[lower] + (sorted_values[upper] - sorted_values[lower]) * fraction


def run(
    url: str,
    duration: float,
    concurrency: int,
    start_gate: Callable[[], None] | None = None,
) -> dict[str, float | int]:
    parsed = urlsplit(url)
    if parsed.scheme != "http" or not parsed.hostname:
        raise ValueError("load generator only supports http URLs")
    path = parsed.path or "/"
    if parsed.query:
        path = f"{path}?{parsed.query}"

    start_event = threading.Event()
    stop_event = threading.Event()
    ready = threading.Barrier(concurrency + 1)
    latencies: list[list[float]] = [[] for _ in range(concurrency)]
    requests = [0] * concurrency
    errors = [0] * concurrency

    def worker(index: int) -> None:
        connection = http.client.HTTPConnection(parsed.hostname, parsed.port or 80, timeout=5)
        ready.wait(timeout=15)
        start_event.wait()
        while not stop_event.is_set():
            started = time.perf_counter()
            try:
                connection.request("GET", path)
                response = connection.getresponse()
                response.read()
                if response.status != 200:
                    errors[index] += 1
                else:
                    requests[index] += 1
                    latencies[index].append(time.perf_counter() - started)
            except (OSError, http.client.HTTPException):
                errors[index] += 1
                connection.close()
                connection = http.client.HTTPConnection(
                    parsed.hostname, parsed.port or 80, timeout=5
                )
        connection.close()

    threads = [
        threading.Thread(target=worker, args=(index,), name=f"client-{index}")
        for index in range(concurrency)
    ]
    for thread in threads:
        thread.start()
    ready.wait(timeout=15)
    if start_gate is not None:
        start_gate()
    started = time.perf_counter()
    start_event.set()
    stop_event.wait(duration)
    stop_event.set()
    for thread in threads:
        thread.join(timeout=10)
        if thread.is_alive():
            raise RuntimeError(f"{thread.name} failed to stop")
    elapsed = time.perf_counter() - started

    values = sorted(value for per_thread in latencies for value in per_thread)
    count = sum(requests)
    return {
        "operations": count,
        "errors": sum(errors),
        "elapsed_seconds": elapsed,
        "throughput": count / elapsed,
        "latency_p50_ms": percentile(values, 0.50) * 1_000,
        "latency_p95_ms": percentile(values, 0.95) * 1_000,
        "latency_p99_ms": percentile(values, 0.99) * 1_000,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", required=True)
    parser.add_argument("--duration", type=float, required=True)
    parser.add_argument("--concurrency", type=int, default=32)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--ready-file", type=Path)
    parser.add_argument("--start-file", type=Path)
    args = parser.parse_args()

    if bool(args.ready_file) != bool(args.start_file):
        parser.error("--ready-file and --start-file must be used together")

    def start_gate() -> None:
        if args.ready_file is None or args.start_file is None:
            return
        args.ready_file.touch()
        deadline = time.monotonic() + 120
        while not args.start_file.exists():
            if time.monotonic() >= deadline:
                raise TimeoutError(f"timed out waiting for {args.start_file}")
            time.sleep(0.01)

    result = run(args.url, args.duration, args.concurrency, start_gate)
    temporary = args.output.with_suffix(".tmp")
    temporary.write_text(json.dumps(result, sort_keys=True))
    os.replace(temporary, args.output)


if __name__ == "__main__":
    main()

