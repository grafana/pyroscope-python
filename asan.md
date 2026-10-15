# ASAN builds

`docker/asan.Dockerfile` builds CPython, the C++ profilers, and the Rust extension (including std) with AddressSanitizer. Test-only: fresh toolchains, no glibc or manylinux constraints.

- `runtime` target: CPython from the `PYTHON_REF` branch (default `3.14`), `--enable-shared --with-address-sanitizer --without-pymalloc`, clang 22 on Ubuntu 26.04.
- `wheel` target: a `linux_*` wheel built with that interpreter, Rust nightly `-Zsanitizer=address` with `build-std`, C/C++ with `-fsanitize=address`. All of it uses recoverable checks.

## Running

```
make asan/test                                  # image + wheel + full integration suite
make asan/test RUN='^TestPythonStackProfiler'   # subset
make asan/image                                 # pyroscope-python-asan:3.14
make asan/wheel                                 # dist/asan/*.whl
make asan/test ASAN_PYTHON_REF=3.13             # another CPython branch or tag
```

`asan/test` runs the regular integration tests with `PYTHON_IMAGE=pyroscope-python-asan:<ref>`, `PYROSCOPE_WHEEL_DIR=dist/asan` and `PYROSCOPE_WHEEL_GLOB='*.whl'`.

## Fast copy warmup

The stack profiler reads with `process_vm_readv` for the first `fast_copy_warmup` seconds (default 15) and switches to `safe_memcpy` after. ASAN only sees `safe_memcpy` reads, so the warmup hides everything that happens early in a run, and most integration tests finish before it ends.

`make asan/test` disables it: `ASAN_FAST_COPY_WARMUP` (default `0`) is passed to the stack workloads as `PYROSCOPE_FAST_COPY_WARMUP`, which they apply with `pyroscope.configure_experimental_stack_profiler(fast_copy_warmup=...)`. The signal handler scenarios keep their own warmups because they test warmup behavior. In your own scripts, call this before `configure()`:

```python
pyroscope.configure_experimental_stack_profiler(fast_copy_warmup=0.0)
```

## ASAN_OPTIONS

The image sets `detect_leaks=0:handle_segv=0:handle_sigbus=0`.

- `handle_segv=0:handle_sigbus=0`: the profiler installs and chains its own SIGSEGV/SIGBUS handlers, and the signal handler tests assert on them. Bad reads are still reported by the instrumentation before they fault.
- Stack-use-after-return detection stays on (the default). It catches reads of returned C stack frames such as `_PyEval_EvalFrameDefault`'s `entry` frame.
- Add `halt_on_error=0` to keep running after a report.
- Add `suppress_equal_pcs=0` too. With `halt_on_error=0`, ASAN reports each faulting pc once, and every stack profiler read goes through the single `memcpy` in `safe_memcpy`. So the first report, usually the short-string over-read in `pyunicode_to_utf8`, hides every later bad read from any caller, including freed frames and wrong code objects. The same applies to the py-spy wrapper, whose reports all come from one call site.

## py-spy reads

py-spy reads through `process_vm_readv`, which ASAN cannot see. The `kk/asan-process-memory` branch of grafana/pyroscope-py-spy reports these reads through `__asan_region_is_poisoned` and `__asan_report_load_n_noabort` when built with ASAN. To build against a local py-spy checkout:

```
rsync -a --exclude target --exclude .git ~/pyroscope-py-spy/ /tmp/pyspy-ctx/
make asan/wheel ASAN_PYSPY_DIR=/tmp/pyspy-ctx
```

The named build context ignores `.dockerignore`, so copy the tree without `target/`. `Cargo.lock` is updated inside the image only.

## Blind spots

- Frames in CPython's data stack (regular Python to Python calls) are not poisoned when popped, so stale reads of them are invisible. Only `entry` frames on the C stack and frames inside freed generator objects get reported.
- ASAN delays reuse of freed memory and returned stack slots, so stale reads usually return the old, still valid data. Expect ASAN reports, not corrupted frame names.
