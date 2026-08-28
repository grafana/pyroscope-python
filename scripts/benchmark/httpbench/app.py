"""The WSGI application under test.

Fixed work per request: the benchmark is fixed-request-count, so a request has
to cost a deterministic amount or "same number of requests" stops meaning "same
work". The handler does what a small JSON endpoint does -- a bounded CPU slice
and a serialisation -- under a realistic call depth, because the sampler's cost
per tick is a function of stack depth.
"""

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from workloads import SHALLOW_DEPTH, _burn, _nest

# Work per request. Sized so a request costs enough to be worth profiling, stays
# short enough that enough of them fit in a measured region, and -- the binding
# constraint -- leaves the server's achievable rate far enough below the load
# generator's ceiling that the generator cannot be the thing being measured. At
# 3000 the server reached 7.2k rps against a 20.6k ceiling, only 2.85x, and the
# headroom guard correctly refused to rank the result.
REQUEST_WORK = int(os.environ.get("BENCH_REQUEST_WORK", "7000"))
# Blocking wait on the IO route, standing in for a downstream call.
IO_WAIT_S = float(os.environ.get("BENCH_REQUEST_IO_WAIT_S", "0.002"))


def _handle(path):
    if path == "/io":
        _nest(SHALLOW_DEPTH, _burn, REQUEST_WORK // 8)
        time.sleep(IO_WAIT_S)
    else:
        _nest(SHALLOW_DEPTH, _burn, REQUEST_WORK)
    return {"ok": True, "pid": os.getpid(), "path": path}


def app(environ, start_response):
    path = environ.get("PATH_INFO", "/")

    if path == "/healthz":
        body = json.dumps({"pid": os.getpid()}).encode()
    else:
        body = json.dumps(_handle(path)).encode()

    start_response("200 OK", [
        ("Content-Type", "application/json"),
        ("Content-Length", str(len(body))),
    ])
    return [body]
