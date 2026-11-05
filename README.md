# pyroscope-python

Pyroscope continuous profiling agent for Python applications.

Uses [py-spy](https://github.com/benfred/py-spy) for stack sampling and the [pyroscope](https://crates.io/crates/pyroscope) Rust crate to send profiles to a Pyroscope server.

## Installation

```bash
pip install pyroscope-io
```

For source builds, initialize the native dependency submodules with
`git submodule update --init --recursive`. Cargo uses the `py-spy` submodule
as a local path dependency; its version is pinned by the parent repository.

## Signal handler requirements

The signal-based Python stack unwinder and deferred resolver use kindasafe's
`SIGSEGV` and `SIGBUS` handlers to recover from failed memory reads. SDK users are
responsible for ensuring that their application and its libraries do not change these
handlers during initialization or after kindasafe installs them, including across profiler stops
and restarts. Install any other handlers before initializing the profiler; handlers
that kindasafe chains to must support direct chaining and remain loaded.

The SDK assumes recovery remains available after successful initialization. It does
not detect handler replacement or reinstall handlers on restart. Replacing these
handlers can cause a profiling memory read to crash the process. Recovery must be
successfully initialized before sampling, and SDK users must ensure `SIGSEGV` and
`SIGBUS` remain unblocked on the resolver thread and on the sampled thread throughout
unwinding, including when sampling interrupts another signal handler. These reads do
not check initialization or signal masks; a failed read in that state can crash the
process. Kindasafe reads use direct memory loads
without syscalls on the successful memory-load path.

Deferred name and line resolution uses py-spy's decoder with kindasafe memory reads
after Python resumes. These reads assume source ranges do not overlap unrelated live
Rust exclusive borrows, including after allocation reuse. Fault recovery does not
establish this condition. Type and
length validation cannot prevent all incorrect attribution from reused addresses or
inconsistent reads.

Signal profiles include a synthetic single-frame stack named `[dropped: REASON]` for
each nonzero drop reason. These stacks are added once when each pprof is built, weighted
by the number of dropped samples (converted to CPU time using the sampling period).
Counters are consumed at that point, so drops are not repeated in later profiles.

## License

Apache-2.0

### Running integration tests

The integration suite runs Python and a real Pyroscope server as native processes
on Linux and macOS. It uses the same workloads and profile assertions on both
platforms. Each test gets isolated backend storage, loopback ports, and process
groups that are stopped during cleanup.

With Go, Rust, CMake, a C++ compiler, and the target Python installed:

```sh
python3 -m venv .venv
. .venv/bin/activate
make -C integration-test setup PYTHON=python
make -C integration-test test PYTHON=python
# Or run one suite:
make -C integration-test test/TestPythonSignalHandlerSuites PYTHON=python
```

`setup` builds and installs the native wheel and downloads Pyroscope 2.3.1 using
`wget` and `tar`. To download only the backend, run
`make -C integration-test download-pyroscope`. The binary is stored in
`integration-test/.tmp/pyroscope`; Make skips the download when it already exists.
Set `PYROSCOPE_BINARY` to use an existing backend executable when running tests.
CPU timer tests require Linux and CPython 3.12+; macOS and older Python versions
run the fallback tests instead. CI covers Python 3.10–3.15 on both architectures
of Linux and macOS. Release manylinux/musllinux packaging remains separate from
these native tests; the native Linux jobs exercise glibc, not musl.

The Linux ASAN job also uses this native harness. `make asan/test
ASAN_PYTHON_REF=3.12` builds an instrumented CPython and SDK, requiring Clang/LLVM
18, CPython build dependencies, and Rust nightly-2026-10-25 with rust-src.
CPython sources and builds are cached separately for each version under
`.tmp/cpython-asan/310/` through `.tmp/cpython-asan/315/`. Run
`make asan/clone ASAN_PYTHON_REF=3.15` to clone only, or
`make asan/python ASAN_PYTHON_REF=3.15` to build and install the interpreter.
These targets use completion files to skip successful clones and builds on later
runs. `ASAN_PYTHON_REF` supports each version from 3.10 through 3.15.
