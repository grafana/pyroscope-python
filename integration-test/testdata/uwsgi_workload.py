import uwsgi
from http_workload import application as application
from http_workload import shutdown, start_profiler

# --lazy-apps imports this separately in each worker after uWSGI installs its
# handlers. --enable-threads lets the profiler's background threads run and
# --single-interpreter keeps one CPython VM. Preserve the recovery handlers.
start_profiler(uwsgi.worker_id())
uwsgi.atexit = shutdown
