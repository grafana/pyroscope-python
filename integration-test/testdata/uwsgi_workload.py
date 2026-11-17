import json
import os

import pyroscope
import uwsgi

# --lazy-apps imports this module separately in each worker, after uWSGI has
# installed its recovery signal handler. Never initialize in the master or
# replace SIGSEGV/SIGBUS handlers after configure(). --enable-threads lets the
# profiler's background threads run; --single-interpreter keeps one CPython VM.
if not pyroscope.configure(
    application_name=os.environ["PYROSCOPE_APPLICATION_NAME"],
    server_address=os.environ["PYROSCOPE_SERVER_ADDRESS"],
    cpu_implementation=getattr(
        pyroscope.ProfilerImplementation, os.environ["CPU_IMPLEMENTATION"]
    ),
    **(
        {"signal_timer": getattr(pyroscope.SignalTimer, os.environ["SIGNAL_TIMER"])}
        if os.environ["CPU_IMPLEMENTATION"] == "Signal"
        else {}
    ),
    sample_rate=100,
    oncpu=True,
    gil_only=True,
    upload_interval=1,
    tags={
        "canary": os.environ["CANARY"],
        "worker": str(uwsgi.worker_id()),
        "worker_pid": str(os.getpid()),
    },
):
    raise RuntimeError("configure() returned False")


def shutdown():
    if not pyroscope.shutdown():
        raise RuntimeError("shutdown() returned False")
    # One write keeps the two workers' shutdown records from interleaving.
    os.write(
        1,
        f"profiler stopped in worker {uwsgi.worker_id()} pid {os.getpid()}\n".encode(),
    )


uwsgi.atexit = shutdown


def burn_cpu(value):
    # Fixed work and a deterministic response; no sleep or I/O in the hot loop.
    for _ in range(1_000_000):
        value = (value * 1664525 + 1013904223) & 0xFFFFFFFF
    return value


def first_branch():
    return burn_cpu(1)


def second_branch():
    return burn_cpu(2)


def workload():
    return first_branch() + second_branch()


def application(environ, start_response):
    path = environ["PATH_INFO"]
    if path == "/ready":
        body = b"ready\n"
    elif path == "/work":
        body = json.dumps(
            {"worker": uwsgi.worker_id(), "pid": os.getpid(), "result": workload()}
        ).encode()
    else:
        start_response("404 Not Found", [("Content-Length", "0")])
        return []
    start_response("200 OK", [("Content-Length", str(len(body)))])
    return [body]
