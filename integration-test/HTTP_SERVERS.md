# HTTP server profiling integration tests

The tests run real HTTP servers with two worker processes, send requests for
15 seconds, stop the workers, and query the profiles uploaded to Pyroscope.
The default is CPython 3.15; `PYTHON_VERSION` and the existing CI platform matrix
select other supported interpreters. Signal uses its existing CPython 3.11–3.14
guard, while py-spy and Stack also run on 3.15.

| Test | Server and workload | Profiler lifecycle |
| --- | --- | --- |
| `TestPythonProfilerGunicorn` | Gunicorn 27.0.1, two sync workers, WSGI | `post_worker_init` starts profiling after worker signal setup; `worker_exit` flushes it. |
| `TestPythonProfilerUvicorn` | Uvicorn 0.54.0, two spawn workers, ASGI with asyncio/h11 | Each worker starts and stops profiling through ASGI lifespan. |
| `TestPythonProfilerUWSGI` | Embedded Python plugin and separate `dlopen` plugin | Lazy application loading initializes profiling after fork; `uwsgi.atexit` flushes it. |

Gunicorn, Uvicorn, Click and h11 are pinned by version and wheel hash in
`testdata/http-servers-requirements.txt`. Their fixture image installs only
binary distributions; all four packages have universal Python wheels, so these
server fixtures require no native compilation. The SDK wheel is built separately
and installed offline. See [UWSGI.md](UWSGI.md) for the native uWSGI fixture.

`testdata/http_workload.py` supplies the shared CPU loops, response calculation,
profiling configuration and shutdown record. Gunicorn and Uvicorn identify
workers by PID; uWSGI supplies its worker number. The common Go driver verifies
responses from exactly two distinct worker PIDs, rejects restarts, and disables
connection reuse so both clients cannot remain attached to one Uvicorn worker.
Readiness, HTTP timeouts, graceful shutdown, per-worker flush records and the
existing container/network cleanup helpers apply to all three servers.

Each request does identical work through `first_branch;burn_cpu` and
`second_branch;burn_cpu`. For each worker, the complete expected stacks must each
carry 35–65% of total sample weight, together cover at least 90%, and have at
least 2,000,000,000 ns total weight (200 periods at 100 Hz). The 15 percentage
point tolerance permits sampling/scheduling noise; the remaining 10% permits
startup, HTTP bookkeeping and Uvicorn's worker healthcheck thread. All unexpected
stacks remain in the denominator, and diagnostics print the complete profile.

The prefixes in `httpServerStackPrefix` come from inspected uploaded profiles:

- Gunicorn preserves the console entry point and master/fork frames underneath
  `Worker.init_process` and the sync worker's request handling frames.
- Uvicorn preserves `spawn_main`, process startup, `Process.target`, the asyncio
  event loop and `RequestResponseCycle.run_asgi`. On CPython 3.11 its compatibility
  wrapper is named `asyncio_run`; on 3.12–3.15 the frame is `run`. ASGI coroutine
  frames are included all the way through `asgi_application` to the CPU leaf.
- uWSGI calls `application` directly from C, so its Python request stacks start
  there. Both uWSGI loading modes reuse the same WSGI function.

Run from the repository root:

```sh
make -C integration-test test/TestPythonProfilerGunicorn
make -C integration-test test/TestPythonProfilerUvicorn
PYTHON_VERSION=3.14 make -C integration-test test/TestPythonProfilerUvicorn PROFILER=signal
```

`PROFILER` accepts `all`, `py-spy`, `signal`, `stack`, and the `dd-trace-py` alias.
When wheels already exist, pass `PYROSCOPE_WHEEL_DIR="$PWD/dist"` to make, or run
Go directly from `integration-test`:

```sh
go test -v -timeout 20m -count=1 -run '^TestPythonProfiler(Gunicorn|Uvicorn)$' . -profiler=all
```

The regular CI invocation automatically discovers both new tests. ASAN and
ARM64 were not run locally for this expansion. Gunicorn gthread/gevent workers,
Uvicorn uvloop/httptools, and Gunicorn's ASGI worker are outside these cases.

## Local validation, 2026-11-18

Merged upstream `ea21e0c` into the existing draft branch before validation and
rebuilt both wheel families with its updated kindasafe dependency. The SDK's
OpenSSL 3.5.9 pin is unchanged. Commands from the repository root used the local
Go 1.26.10 installation and the Docker wrapper described in [UWSGI.md](UWSGI.md):

```sh
export PATH="$PWD/.cache/bin:$PWD/.cache/go/bin:$PATH"
make linux/amd64
docker buildx build --network=host --build-arg=PLATFORM=x86_64 \
  --platform=linux/amd64 --output=. -f docker/wheel-musllinux.Dockerfile .
```

The first captures used `-profiler=py-spy` on CPython 3.11 and 3.15 to inspect
both servers' full stacks before finalizing the assertions. They passed in
43.195s and 44.476s respectively. The subsequent matrix used this command from
`integration-test`, substituting each Python version and image suffix:

```sh
PYTHON_VERSION=3.14 PYTHON_IMAGE_SUFFIX=slim \
  PYROSCOPE_TEST_LOG_DIR="$PWD/../.cache/http-server-validation/containers-3.14-slim" \
  go test -v -timeout 20m -count=1 -run '^TestPythonProfiler(Gunicorn|Uvicorn)$' . -profiler=all
```

For 3.11 and 3.15 on both libc variants, the same command included uWSGI
regressions by using `-run '^TestPythonProfiler(Gunicorn|Uvicorn|UWSGI)$'`.
Full test output, container logs and collapsed profiles are retained locally in
`.cache/http-server-validation/` (ignored by Git).

| Python | Gunicorn + Uvicorn cases per libc | uWSGI regression cases per libc | glibc (`slim`) | musl (`alpine`) |
| --- | --- | --- | --- | --- |
| 3.11 | 8 passed | 8 passed | PASS, 321.348s | PASS, 333.710s |
| 3.12 | 8 passed | Not rerun | PASS, 177.521s | PASS, 168.290s |
| 3.13 | 8 passed | Not rerun | PASS, 177.350s | PASS, 163.637s |
| 3.14 | 8 passed | Not rerun | PASS, 162.610s | PASS, 165.314s |
| 3.15 | 4 passed, 4 skipped | 4 passed, 4 skipped | PASS, 163.145s | PASS, 168.193s |

All **72 new server cases and 24 uWSGI regression cases passed**. The 16 skips
are the existing Signal/Python 3.15 exclusions. Inspected 192 worker profiles:

| Server | Worker profiles | Minimum combined workload share | Minimum total sample weight |
| --- | --- | --- | --- |
| Gunicorn | 72 | 98.86% | 9.00 seconds |
| Uvicorn | 72 | 98.39% | 9.07 seconds |
| uWSGI | 48 | 99.66% | 8.99 seconds |

These are complete example Stack profiles, including the small amount of
non-workload weight (values in nanoseconds). Gunicorn, Python 3.12/musl,
worker PID 16:

```text
;<module>;run;Application.run;BaseApplication.run;Arbiter.run;Arbiter.manage_workers;Arbiter.spawn_workers;Arbiter.spawn_worker;Worker.init_process;SyncWorker.run;SyncWorker.run_for_one;SyncWorker.accept;SyncWorker.handle;SyncWorker.handle_request;Response.write;Response.send_headers;write 51378000
;<module>;run;Application.run;BaseApplication.run;Arbiter.run;Arbiter.manage_workers;Arbiter.spawn_workers;Arbiter.spawn_worker;Worker.init_process;SyncWorker.run;SyncWorker.run_for_one;SyncWorker.accept;SyncWorker.handle;SyncWorker.handle_request;application;workload;first_branch;burn_cpu 7544789000
;<module>;run;Application.run;BaseApplication.run;Arbiter.run;Arbiter.manage_workers;Arbiter.spawn_workers;Arbiter.spawn_worker;Worker.init_process;SyncWorker.run;SyncWorker.run_for_one;SyncWorker.accept;SyncWorker.handle;SyncWorker.handle_request;application;workload;second_branch;burn_cpu 7575387000
```

Uvicorn, Python 3.12/glibc, worker PID 11:

```text
;Thread._bootstrap;Thread._bootstrap_inner;Thread.run;Process.always_pong;Process.pong;_ConnectionBase.send;Connection._send_bytes;Connection._send 5365000
;<module>;spawn_main;_main;BaseProcess._bootstrap;BaseProcess.run;subprocess_started;Process.target;Server.run;run;Runner.run;BaseEventLoop.run_until_complete;BaseEventLoop.run_forever;BaseEventLoop._run_once;Handle._run;RequestResponseCycle.run_asgi;asgi_application;RequestResponseCycle.send;_SelectorSocketTransport.write 58736000
;<module>;spawn_main;_main;BaseProcess._bootstrap;BaseProcess.run;subprocess_started;Process.target;Server.run;run;Runner.run;BaseEventLoop.run_until_complete;BaseEventLoop.run_forever;BaseEventLoop._run_once;Handle._run;RequestResponseCycle.run_asgi;asgi_application;workload;first_branch;burn_cpu 7427900000
;<module>;spawn_main;_main;BaseProcess._bootstrap;BaseProcess.run;subprocess_started;Process.target;Server.run;run;Runner.run;BaseEventLoop.run_until_complete;BaseEventLoop.run_forever;BaseEventLoop._run_once;Handle._run;RequestResponseCycle.run_asgi;asgi_application;workload;second_branch;burn_cpu 7314492000
```

Default-version and selector checks, from `integration-test`:

```sh
env -u PYTHON_VERSION -u PYTHON_IMAGE -u PYTHON_IMAGE_SUFFIX \
  PYROSCOPE_TEST_LOG_DIR="$PWD/../.cache/http-server-validation/containers-default-alias" \
  go test -v -timeout 10m -count=1 -run '^TestPythonProfiler(Gunicorn|Uvicorn)$' . -profiler=dd-trace-py
# PASS, 40.361s; only the two Stack cases run, using the CPython 3.15 default
env -u PYTHON_VERSION -u PYTHON_IMAGE -u PYTHON_IMAGE_SUFFIX \
  go test -v -count=1 -run '^TestPythonProfiler(Gunicorn|Uvicorn)$' . -profiler=signal
# PASS, 0.005s; all cases skip on 3.15 without Docker access
go test -count=1 -run '^TestConfigurationProfileShape$' ./...
go vet ./...
```

The focused Go checks, gofmt, Ruff format/import checks for all three workload
files and `git diff --check` passed. Test containers and temporary image tags
were cleaned up by the harness.
