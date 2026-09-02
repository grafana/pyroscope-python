"""Gunicorn setup from the Pyroscope docs: sync workers, agent started per
worker in post_fork."""
import os
import threading

bind = "127.0.0.1:0"
workers = int(os.environ.get("GUNICORN_WORKERS", "8"))
worker_class = "sync"
timeout = 0
graceful_timeout = 5
preload_app = os.environ.get("PRELOAD", "0") == "1"


def post_fork(server, worker):
    import workload

    workload.configure()

    stop = threading.Event()
    for _ in range(int(os.environ.get("THREADS", "4"))):
        threading.Thread(target=workload.churn, args=(stop, 30), daemon=True).start()
