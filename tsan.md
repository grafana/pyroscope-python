# TSAN builds

`docker/tsan.Dockerfile` builds CPython, the C++ profilers, and the Rust extension (including std) with ThreadSanitizer. Test-only: fresh toolchains, no glibc or manylinux constraints.

- `runtime` target: CPython from the `PYTHON_REF` branch (default `3.14`), `--enable-shared --with-thread-sanitizer`, clang 22 on Ubuntu 26.04.
- `wheel` target: a `linux_*` wheel built with that interpreter, Rust nightly `-Zsanitizer=thread` with `build-std`, C/C++ with `-fsanitize=thread`.

## Running

```
make tsan/test                                  # image + wheel + full integration suite
make tsan/test RUN='^TestPythonStackProfiler'   # subset
make tsan/image                                 # pyroscope-python-tsan:3.14
make tsan/wheel                                 # dist/tsan/*.whl
make tsan/test TSAN_PYTHON_REF=3.13             # another CPython branch or tag
```

Tested with CPython 3.13 and 3.14. `--with-thread-sanitizer` does not exist before 3.13.

`tsan/test` runs the regular integration tests with `PYTHON_IMAGE=pyroscope-python-tsan:<ref>`, `PYROSCOPE_WHEEL_DIR=dist/tsan` and `PYROSCOPE_WHEEL_GLOB='*.whl'`. 
## Fast copy warmup

The stack profiler reads with `process_vm_readv` for the first `fast_copy_warmup` seconds (default 15) and switches to `safe_memcpy` after. TSAN only sees `safe_memcpy` reads, so the warmup hides everything that happens early in a run, and most integration tests finish before it ends.

`make tsan/test` disables it: `TSAN_FAST_COPY_WARMUP` (default `0`) is passed to the stack workloads as `PYROSCOPE_FAST_COPY_WARMUP`, which they apply with `pyroscope.configure_experimental_stack_profiler(fast_copy_warmup=...)`. The signal handler scenarios keep their own warmups because they test warmup behavior. In your own scripts, call this before `configure()`:

```python
pyroscope.configure_experimental_stack_profiler(fast_copy_warmup=0.0)
```

## ASLR

TSAN needs a fixed memory layout. With `vm.mmap_rnd_bits` above 28 it re-executes itself with ASLR disabled, and Docker's default seccomp profile blocks that `personality` call. The workflow sets `vm.mmap_rnd_bits=28` on the runner before the tests start. Do the same on a local Linux host if the workload fails with `unexpected memory mapping`.

## TSAN_OPTIONS

The image sets `halt_on_error=0:exitcode=0:die_after_fork=0:handle_segv=0:handle_sigbus=0:suppressions=/usr/local/share/tsan/suppressions.txt`.

- `halt_on_error=0:exitcode=0`: reports go to stderr and do not change the exit code. Set `exitcode=66` to make a report fail the test.
- `die_after_fork=0`: the fork tests start profiler threads in a child of a multi-threaded process. TSAN kills such a child by default.
- `handle_segv=0:handle_sigbus=0`: the profiler installs and chains its own SIGSEGV/SIGBUS handlers, and the signal handler tests assert on them.
- `suppressions`: CPython's `Tools/tsan/suppressions.txt` from the same ref, for known races in the interpreter.

## Blind spots

- py-spy and the stack profiler warmup read through `process_vm_readv`. The kernel does the copy, so TSAN does not see these reads.
- TSAN delivers asynchronous signals at the next interceptor or blocking call, not at the instruction where they arrive. Signal timing in the stack profiler is different from a regular build.
