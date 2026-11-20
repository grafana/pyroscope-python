import json
import os

import pyroscope

worker_id = 0


def start_profiler(identity=None):
    global worker_id
    identity = os.getpid() if identity is None else identity
    # Server hooks call this after worker creation and signal-handler setup.
    # Recovery handlers must remain installed and SIGSEGV/SIGBUS unblocked.
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
            "worker": str(identity),
            "worker_pid": str(os.getpid()),
        },
    ):
        raise RuntimeError("configure() returned False")
    worker_id = identity


def shutdown():
    # Gunicorn can invoke worker_exit after a failure during worker startup.
    if not worker_id:
        return
    if not pyroscope.shutdown():
        raise RuntimeError("shutdown() returned False")
    # One write keeps the two workers' shutdown records from interleaving.
    os.write(1, f"profiler stopped in worker {worker_id} pid {os.getpid()}\n".encode())


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


def response_body(result):
    return json.dumps(
        {"worker": worker_id, "pid": os.getpid(), "result": result}
    ).encode()


def application(environ, start_response):
    path = environ["PATH_INFO"]
    if path == "/ready":
        body = b"ready\n"
    elif path == "/work":
        body = response_body(workload())
    else:
        start_response("404 Not Found", [("Content-Length", "0")])
        return []
    start_response("200 OK", [("Content-Length", str(len(body)))])
    return [body]


async def asgi_application(scope, receive, send):
    if scope["type"] == "lifespan":
        # Uvicorn runs lifespan separately in each spawned worker, after its
        # signal handlers are installed and before serving HTTP requests.
        while True:
            message = await receive()
            if message["type"] == "lifespan.startup":
                start_profiler()
                await send({"type": "lifespan.startup.complete"})
            elif message["type"] == "lifespan.shutdown":
                shutdown()
                await send({"type": "lifespan.shutdown.complete"})
                return
    elif scope["type"] == "http":
        status = 200
        if scope["path"] == "/ready":
            body = b"ready\n"
        elif scope["path"] == "/work":
            # Intentionally CPU-bound Python on the event-loop thread so the
            # captured stack includes the real ASGI request coroutine.
            body = response_body(workload())
        else:
            status, body = 404, b""
        await send(
            {
                "type": "http.response.start",
                "status": status,
                "headers": [(b"content-length", str(len(body)).encode())],
            }
        )
        await send({"type": "http.response.body", "body": body})
    else:
        raise RuntimeError(f"unsupported ASGI scope: {scope['type']}")
