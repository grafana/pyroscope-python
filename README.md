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

## Signal stack benchmarks

The Criterion harness embeds CPython, attaches to it, and calls Python functions that
build one fixed mixed stack. Its leaf calls back into Rust once; Rust runs the
entire measurement loop synchronously while the Python stack remains active and the
GIL is held. It calls the production signal profiler's unwinder and resolver directly.

The `mixed` stack has 82 Python frames: the entry point, 32 distinct generated
functions, 32 recursive calls sharing one code object, 16 Python callbacks through
the C-backed `map` iterator, and the leaf that calls Rust.

The three benchmarks are `unwind`, `resolve`, and `unwind_resolve`. Resolution includes
function names, filenames, and current line numbers (`LastInstruction`), including
output allocation and destruction. The resolver and fixed unwind buffer are reused;
resolve-only repeatedly decodes one captured live stack. Before measurement, the
harness checks the entire root-to-leaf sequence against the expected shape and checks
every filename and line against CPython. Interpreter entry/shim frames are skipped by
the production unwinder according to the Python version.

Use a GIL-enabled CPython 3.10–3.14 with development headers and a linkable `libpython`:

```bash
git submodule update --init --recursive
export PYO3_PYTHON="$(command -v python3)"
export Python3_EXECUTABLE="$PYO3_PYTHON"
export Python3_ROOT_DIR="$("$PYO3_PYTHON" -c 'import sys; print(sys.base_prefix)')"
cd rust
cargo bench --bench signal_stack
# Validate the stack and execute each measurement once:
cargo bench --bench signal_stack -- --test
# Measure only one case (standard Criterion filters/options also work):
cargo bench --bench signal_stack -- mixed/unwind_resolve
# Capture a separate native CPU pprof for all three cases (five seconds each):
cargo bench --bench signal_stack -- --profile-time 5
# Or capture just one case:
cargo bench --bench signal_stack -- mixed/resolve --profile-time 5
```

Criterion writes results under `rust/target/criterion` (or `$CARGO_TARGET_DIR/criterion`).
Times are per stack and throughput is stacks per second. Python setup, stack creation,
and the Python-to-Rust transition are outside the timed loop. The Python unwind and
resolve calls always run synchronously. Normal timing runs have no sampling timer;
signal delivery, ring-buffer transport, concurrent Python execution, and stale-object
recovery are not measured. Recovery handlers are initialized and recovery signals
unblocked once before any stack reads.

`--profile-time` uses the `pyroscope` crate from pyroscope-rs, with its native CPU
backend sampling at 100 Hz. Each case gets a fresh sampler and writes
`rust/target/criterion/mixed/<operation>/profile/profile.pprof`. These are native
Rust/C/C++ CPU profiles of the benchmark loop, separate from the Python stacks being
unwound. No server is needed. The sampler stops and clears its samples between cases;
pprof encoding and file writes happen afterward. Profiling mode uses `SIGPROF` to
sample native execution and does not produce Criterion timing comparisons. Bench
builds retain debug information for symbols and source lines. For example:

```bash
go tool pprof -http=:8080 target/criterion/mixed/unwind_resolve/profile/profile.pprof
```

## License

Apache-2.0
