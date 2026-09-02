# Reproducer for grafana/pyroscope-python#37

Random native crashes (SIGSEGV / `free(): invalid pointer`) in processes that
run the Pyroscope Python agent. Reported against 0.8.11/0.8.14 (celery
prefork) and again against 1.2.1 (gunicorn sync workers, Python 3.11).

**CPU profiler only** — the reports predate memory profiling, and everything
here runs with `mem_enabled=False`.

## Status

**Reproduced, with a core dump whose stack is identical to both reports.**

```
#7  free ()                                          from libc.so.6
#8  anyhow::error::object_drop ()                    from pyroscope/_native.cpython-311-x86_64-linux-gnu.so
#9  anyhow::error::object_drop ()                    from pyroscope/_native.cpython-311-x86_64-linux-gnu.so
#10 std::sys::backtrace::__rust_begin_short_backtrace ()
#11 core::ops::function::FnOnce::call_once{{vtable.shim}} ()
#12 <std::sys::thread::unix::Thread>::new::thread_start ()
```

SIGABRT out of glibc's `free()` (`free(): invalid pointer`), i.e. the reports'
`free(): invalid pointer` / `Fatal Python error: Aborted`; other runs died with
a plain SIGSEGV, i.e. the reports' `Worker (pid:56) was sent code 139`.

| | |
|---|---|
| package | `pyroscope-io==1.2.1` (PyPI wheel, unmodified) |
| python | 3.11 (`python:3.11-slim`) |
| profiler | CPU only (`mem_enabled=False`) |
| config | `sample_rate=997`, `oncpu=False`, `gil_only=False`, `report_pid/thread_id/thread_name=True` |
| env | `PYTHONMALLOC=malloc`, `MALLOC_CHECK_=3` |
| load | 8 concurrent worker processes, 60 s each |
| rate | 2 crashes in ~6 process-hours (~1 per 3 process-hours) |

For comparison, the reporter sees ~12 crashes/hour across 60 workers, i.e.
~1 per 5 worker-hours.

Runs that did *not* crash (same workload):

* Python 3.13, default allocator, 200 runs × 60 s (~3.3 process-hours).
* Python 3.11 under valgrind/memcheck (nothing reported; memcheck serializes
  threads, which suppresses the race).
* Amplifier variants that turned out not to help: 16 procs × 16 threads ×
  depth 150 × 2000 Hz (~1.4 process-hours), and a variant that makes py-spy
  fail every single sample so the error path is maximally hot (~0.9
  process-hours, `FRAME_CYCLE=1`).

`PYTHONMALLOC=malloc` is not the default, but it is a legitimate setting and it
does not introduce the bug: it puts CPython's objects in the same glibc heap
the profiler uses, so a stray write or stale free is far more likely to hit
something that glibc notices. `MALLOC_CHECK_=3` only adds detection.

## Root cause

See **[ROOT_CAUSE.md](ROOT_CAUSE.md)** for the full analysis, disassembly and
evidence. In short:

`remoteprocess::ProcessMemory::copy_struct` builds a `T` out of bytes read
from the profiled process with `ptr::read`, and `T: Copy` does not mean every
bit pattern is a valid `T`. On Python 3.11 py-spy's `_PyInterpreterFrame`
contains `is_entry: bool` at offset 68, which rustc uses as the *niche
discriminant* of the returned `Result<_PyInterpreterFrame,
remoteprocess::Error>`:

```
sizeof(Result<_PyInterpreterFrame, remoteprocess::Error>) == 80
sizeof(_PyInterpreterFrame)                               == 80    # no room for a tag
```

`bool` and `c_char` have the same size; what differs is that `bool` has 254
spare bit patterns for rustc to steal. Hence 3.12+ (`c_char` in that position)
get a dedicated tag word instead: `sizeof(Result<frame, Error>)` is 88 vs a
80-byte frame on 3.12, and 96 vs 88 on 3.14.

The shipped wheel encodes `Err` by writing `2` into that byte
(`movb $0x2,0x44(%rbx)`), and the `Ok` path copies all 80 target bytes
verbatim. So a stale frame read whose byte 68 is `2` turns into an
`Err(remoteprocess::Error)` whose payload is 32 bytes of interpreter memory —
and dropping that error in pyroscope's consumer thread calls `free()` on
whatever those bytes contain, which is the reported `free(mem=0x18)`.

`poc_niche_confusion.py` plants that condition and crashes with exit 139 in
seconds (control run with `NO_PROFILER=1` survives).

## Layout

| file | what it is |
|---|---|
| `ROOT_CAUSE.md` | the full analysis: disassembly, core-dump evidence, fix directions |
| `workload.py` | the interpreter churn described above; profiler config from env |
| `runner.py` | runs N workers in rounds until one dies, keeps logs + cores |
| `sink.py` / `tls_sink.py` | ingest sinks (plain HTTP / HTTPS with a self-signed cert) |
| `wsgi.py`, `gunicorn_conf.py` | gunicorn sync workers, agent started in `post_fork` (as in the docs) |
| `forkload.py` | fork-shaped variant: profiled parent forks profiled children continuously |
| `Dockerfile` | Python 3.11 + `pyroscope-io==1.2.1` + gdb |
| `Dockerfile.valgrind`, `run-valgrind.sh` | same workload under memcheck |
| `run-asan.sh` | workload against an ASAN build of `rust/` from this repo |
| `poc_niche_confusion.py` | **deterministic reproducer** — plants the bad byte, crashes in seconds |
| `poc_frame_cycle.py` | makes py-spy's frame walk hit its 4096-frame bound every sample |
| `poc_ucs4_garbage.py`, `poc_hostile_string.py` | targeted probes of py-spy's UCS-4 decode path |
| `poc_shutdown_race.py` | configure()/shutdown() cycles under load |
| `Dockerfile.debug`, `Dockerfile.guard` | debug-info build, and the guarding-allocator build (`rust/src/debug_alloc.rs`, feature `debug-alloc`) |
| `analyze-core.sh` | batch gdb backtraces for every core under `artifacts/` |
| `artifacts*/` | the core dumps referenced in ROOT_CAUSE.md |

## Running it

Docker (the configuration that crashed), cores land in `./artifacts`:

```sh
docker build -t pyroscope-repro37:py311 .
mkdir -p artifacts
docker run --rm --ulimit core=-1 -v "$PWD/artifacts:/cores" -e CORES_DIR=/cores \
  -e MALLOC_CHECK_=3 -e PYTHONMALLOC=malloc -e SAMPLE_RATE=997 \
  -e ONCPU=0 -e GIL_ONLY=0 -e REPORT_PID=1 -e REPORT_THREAD_ID=1 -e REPORT_THREAD_NAME=1 \
  pyroscope-repro37:py311 --mode threads --procs 8 --duration 60 --budget 3600
```

Locally:

```sh
python -m venv .venv && .venv/bin/pip install pyroscope-io==1.2.1 gunicorn
MALLOC_CHECK_=3 PYTHONMALLOC=malloc SAMPLE_RATE=997 ONCPU=0 GIL_ONLY=0 \
  .venv/bin/python runner.py --mode threads --procs 8 --duration 60 --budget 3600
```

`--mode gunicorn` uses sync workers with the agent started in `post_fork`;
`--mode fork` uses `forkload.py`; `TLS=1` sends the profiles over HTTPS so the
upload path goes through native-tls/OpenSSL as it does against a real endpoint.

A worker log is kept for every run under `logs/`, and each worker runs in its
own directory under `cores/` so a core dump is not overwritten by a sibling.
`runner.py` stops at the first non-zero exit and prints the log tail.

## Under ASAN

Build the extension from this repo with the sanitizer and run the same
workload against it (CPU only, so no C++/CMake in the build):

```sh
rustup toolchain install nightly
cd rust && RUSTFLAGS="-Zsanitizer=address -Cdebuginfo=1" \
    cargo +nightly build --release --locked --target x86_64-unknown-linux-gnu
cp target/x86_64-unknown-linux-gnu/release/lib_native.so \
   ../repro37/asan/pyroscope/_native.so
cd ../repro37 && ./run-asan.sh          # or: runner.py --mode asan
```

rustc ships only the static ASAN runtime and the extension is dlopened, so the
runtime is preloaded from gcc's `libasan.so.8`, which exports the same
interface (`verify_asan_link_order=0`).

## Ruled out so far

* **ABI mismatch** (the original hypothesis in the issue). The extension links
  no Python symbols; 1.2.1 ships a per-version `.so` and still crashes.
* **Memory profiling.** Disabled everywhere here; the crash predates it.
* **py-spy's invalid-`char` UB on its own.** `poc_hostile_string.py` plants a
  well-formed, immortal UCS-4 header as a code object's `co_filename` and
  rewrites its character data with random 32-bit words while the sampler
  decodes it, so *every* sample decodes non-scalar values. That alone does not
  crash (it does produce `String`s holding invalid UTF-8, which is still UB and
  worth fixing). Note the control run matters here: an earlier version of this
  probe let Python free the fake header at shutdown, and that use-after-free —
  not the profiler — was what corrupted the heap.
