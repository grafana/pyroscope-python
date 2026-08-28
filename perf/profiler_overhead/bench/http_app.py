from __future__ import annotations

import os
import time
from collections.abc import Callable, Iterable

from .profilers import start_profiler
from .workloads import (
    STACK_TRACE_DEPTH,
    cpu_step,
    exercise_stack_depth_probe,
    run_with_stack_depth,
)


# Gunicorn imports the application in each worker because preload_app is disabled.
# Keeping the handle alive is sufficient; Pyroscope installs its own atexit shutdown.
_STOP_PROFILER = start_profiler(
    os.environ.get("BENCH_PROFILER", "none"),
    "overhead-gunicorn",
    os.environ.get("BENCH_COLLECTOR", "http://collector:4040"),
)
exercise_stack_depth_probe()


def application(
    environ: dict[str, object],
    start_response: Callable[[str, list[tuple[str, str]]], object],
) -> Iterable[bytes]:
    path = str(environ.get("PATH_INFO", "/"))
    status, body = run_with_stack_depth(STACK_TRACE_DEPTH, _handle_request, path)

    start_response(
        status,
        [
            ("Content-Type", "text/plain"),
            ("Content-Length", str(len(body))),
        ],
    )
    return [body]


def _handle_request(path: str) -> tuple[str, bytes]:
    if path == "/health":
        body = b"ok"
        status = "200 OK"
    elif path == "/cpu":
        value = 1
        for _ in range(12):
            value = cpu_step(value)
        body = str(value).encode()
        status = "200 OK"
    elif path == "/io":
        time.sleep(0.005)
        body = b"ok"
        status = "200 OK"
    else:
        body = b"not found"
        status = "404 Not Found"
    return status, body

