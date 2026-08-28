"""gunicorn configuration.

The agent is started in post_fork and nowhere else. Starting it before the fork
would leave the child holding an inherited Rust sampler thread: lib.rs notes the
agent is not safe to inherit across a fork, and the child-side fork handler does
not stop the sampler. post_fork is the only correct hook, and the harness checks
that every worker actually produced samples so a hook that silently did not fire
shows up as a failure rather than as a cheap result.
"""

import os

bind = os.environ.get("BENCH_BIND", "127.0.0.1:8080")
workers = int(os.environ.get("BENCH_WORKERS", "2"))
threads = int(os.environ.get("BENCH_THREADS", "4"))
worker_class = "gthread"
# Never recycle a worker mid-run: a restart would re-run post_fork, restart the
# agent and put startup cost inside the measured region.
max_requests = 0
keepalive = 30
timeout = 300
graceful_timeout = 60
preload_app = False
accesslog = None
errorlog = "-"
loglevel = "warning"

_agent_started = False


def post_fork(server, worker):
    global _agent_started
    variant = os.environ.get("BENCH_VARIANT", "none")
    if variant == "none":
        return

    import sys
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    from variants import VARIANTS, RunConfig

    v = VARIANTS[variant]
    if v.start is None:
        return
    cfg = RunConfig(
        run_id=os.environ["BENCH_RUN_ID"],
        sink_url=os.environ["BENCH_SINK_URL"],
        app_name="bench-http",
        sample_rate=(int(os.environ["BENCH_SAMPLE_RATE"])
                     if os.environ.get("BENCH_SAMPLE_RATE") else None),
        upload_interval=(int(os.environ["BENCH_UPLOAD_INTERVAL"])
                         if os.environ.get("BENCH_UPLOAD_INTERVAL") else None),
    )
    v.start(cfg)
    _agent_started = True


def worker_exit(server, worker):
    """Flush on the way out.

    shutdown() joins the sampler and upload threads and completes the final
    POST, so the samples from the tail of the run are not silently dropped.
    """
    if not _agent_started:
        return
    from variants import VARIANTS
    try:
        VARIANTS[os.environ.get("BENCH_VARIANT", "none")].stop()
    except Exception as exc:  # noqa: BLE001 - must not block worker teardown
        print(f"worker_exit: shutdown failed: {exc}", flush=True)
