from __future__ import annotations

import argparse
import json
import os
import time
from pathlib import Path

from .profilers import MODES, start_profiler
from .workloads import ThreadedWorkload, exercise_stack_depth_probe, run_threaded


def write_json(path: Path, value: object) -> None:
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, sort_keys=True))
    os.replace(temporary, path)


def wait_for(path: Path, timeout: float) -> None:
    deadline = time.monotonic() + timeout
    while not path.exists():
        if time.monotonic() >= deadline:
            raise TimeoutError(f"timed out waiting for {path}")
        time.sleep(0.01)


def parse_workload(value: str) -> tuple[str, int]:
    try:
        kind, count = value.rsplit("-", 1)
        thread_count = int(count)
    except (ValueError, TypeError) as error:
        raise argparse.ArgumentTypeError("workload must look like cpu-1 or io-4") from error
    if kind not in {"cpu", "io"} or thread_count < 1:
        raise argparse.ArgumentTypeError("workload must look like cpu-1 or io-4")
    return kind, thread_count


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--workload", required=True, type=parse_workload)
    parser.add_argument("--profiler", required=True, choices=sorted(MODES))
    parser.add_argument("--duration", type=float, required=True)
    parser.add_argument("--warmup", type=float, default=1.0)
    parser.add_argument("--control", type=Path, default=Path("/control"))
    parser.add_argument("--collector", default="http://collector:4040")
    parser.add_argument("--io-server", default="http://io-sink:4040")
    args = parser.parse_args()

    kind, thread_count = args.workload
    stop_profiler = start_profiler(
        args.profiler,
        f"overhead-{kind}-{thread_count}",
        args.collector,
    )
    try:
        exercise_stack_depth_probe()
        io_url = f"{args.io_server}/io?delay_ms=1"
        if args.warmup > 0:
            warmup = run_threaded(kind, thread_count, args.warmup, io_url)
            if warmup.errors:
                raise RuntimeError(f"warmup had {warmup.errors} I/O errors")

        workload = ThreadedWorkload(kind, thread_count, io_url)
        workload.prepare()
        write_json(
            args.control / "ready.json",
            {"kind": kind, "threads": thread_count, "profiler": args.profiler},
        )
        wait_for(args.control / "start", timeout=120)
        result = workload.run(args.duration)
        write_json(
            args.control / "result.json",
            {
                "operations": result.operations,
                "errors": result.errors,
                "elapsed_seconds": result.elapsed_seconds,
                "throughput": result.throughput,
            },
        )
        wait_for(args.control / "release", timeout=120)
    finally:
        stop_profiler()


if __name__ == "__main__":
    main()

