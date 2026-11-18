from http_workload import shutdown, start_profiler


def post_worker_init(worker):
    # Worker.init_process has installed Gunicorn's handlers and loaded the app.
    start_profiler()


def worker_exit(server, worker):
    shutdown()
