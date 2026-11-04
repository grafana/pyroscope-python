# ASAN builds

`docker/asan.Dockerfile` builds CPython, the C++ profilers, and the Rust extension (including std) with AddressSanitizer. Test-only: fresh toolchains, no glibc or manylinux constraints.

- `runtime` target: CPython from the `PYTHON_REF` branch (default `3.14`), `--enable-shared --with-address-sanitizer --without-pymalloc`, clang 22 on Ubuntu 26.04.
- `wheel` target: a `linux_*` wheel built with that interpreter, Rust nightly `-Zsanitizer=address` with `build-std`, C/C++ with `-fsanitize=address`. All of it uses recoverable checks.

## Running

```
make asan/test                                  # image + wheel + full integration suite
make asan/test PROFILER=py-spy                   # only py-spy
make asan/test PROFILER=signal                   # only Signal (ITimer and CTimer)
make asan/test PROFILER=stack                    # Stack and memory (vendored dd-trace-py)
make asan/test PROFILER=signal RUN='ITimer$'      # subset
make asan/image                                 # pyroscope-python-asan:3.14
make asan/wheel                                 # dist/asan/*.whl
make asan/test ASAN_PYTHON_REF=3.13             # another CPython branch or tag
```

Tested with CPython 3.10 to 3.15. 3.10 and 3.11 still find system libraries through `setup.py`, which misses the multiarch directories, so the image passes them in `CPPFLAGS` and `LDFLAGS`.

`asan/test` runs the regular integration tests with `PYTHON_IMAGE=pyroscope-python-asan:<ref>`, `PYROSCOPE_WHEEL_DIR=dist/asan` and `PYROSCOPE_WHEEL_GLOB='*.whl'`.

`PROFILER` defaults to `all`. The same selector works directly with Go:
`cd integration-test && go test -v -timeout 60m . -profiler=signal`.
Accepted values are `all`, `py-spy`, `signal`, and `stack`; `dd-trace-py`
is an alias for `stack`. A selected implementation skips unrelated CPU tests.
Memory-only tests run in the `stack` suite and with `all`, so the ASAN matrix
covers the memory profiler. Lifecycle tests run with their existing implementation.
Signal supports CPython 3.10–3.14 and is skipped on other versions.

CI runs a separate job for each implementation at each Python version and
architecture, with `fail-fast: false`. Each job saves the test output and full
container logs as an artifact, including failed runs, for comparing ASAN reports.
Set `PYROSCOPE_TEST_LOG_DIR` to an output directory to save container logs locally.

## ASAN_OPTIONS

The image sets `detect_leaks=0:handle_segv=0:handle_sigbus=0`.

- `handle_segv=0:handle_sigbus=0`: the profiler installs and chains its own SIGSEGV/SIGBUS handlers, and the signal handler tests assert on them. Instrumented loads and explicit read checks still report poisoned memory.
- Stack-use-after-return detection stays on (the default). It catches reads of returned C stack frames such as `_PyEval_EvalFrameDefault`'s `entry` frame.
- Add `halt_on_error=0` to keep running after a report.
- Add `suppress_equal_pcs=0` too. With `halt_on_error=0`, ASAN reports each faulting pc once, and every stack profiler read goes through the single `memcpy` in `safe_memcpy`. So the first report, usually the short-string over-read in `pyunicode_to_utf8`, hides every later bad read from any caller, including freed frames and wrong code objects. The same applies to the py-spy wrapper, whose reports all come from one call site.

## py-spy reads

ASAN builds use the `py-spy` submodule by default, including local edits. Initialize
it with `git submodule update --init --recursive` before building.

py-spy reads through `process_vm_readv`, which ASAN cannot see. The `kk/asan-process-memory` branch of grafana/pyroscope-py-spy reports these reads through `__asan_region_is_poisoned` and `__asan_report_load_n_noabort` when built with ASAN. To replace the submodule's source with a separate local checkout for an ASAN build:

```
rsync -a --exclude target --exclude .git ~/pyroscope-py-spy/ /tmp/pyspy-ctx/
make asan/wheel ASAN_PYSPY_DIR=/tmp/pyspy-ctx
```

The named build context ignores `.dockerignore`, so copy the tree without `target/`.
If that checkout changes dependencies, update `rust/Cargo.lock` to match before building.

## Signal reads

The unwinder and deferred resolver both read through `kindasafe`. Its naked
assembly bypasses compiler instrumentation, so ASAN builds check every successful
`u64` and `slice` read with `__asan_region_is_poisoned` and report the first poisoned
byte through `__asan_report_load_n_noabort`. Failed reads still use signal recovery.
Normal builds have no additional checks.

The ASAN wheel build runs kindasafe's tests, including subprocess regressions for
poisoned aligned/unaligned words, a poisoned region inside a slice, and a read from
a `SIGPROF` handler. Valid, empty, and faulting reads must not emit poison reports.

## Blind spots

- Frames in CPython's data stack (regular Python to Python calls) are not poisoned when popped, so stale reads of them are invisible. Only `entry` frames on the C stack and frames inside freed generator objects get reported.
- ASAN delays reuse of freed memory and returned stack slots, so stale reads usually return the old, still valid data. Expect ASAN reports, not corrupted frame names.
