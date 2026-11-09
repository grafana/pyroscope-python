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

## Experimental native worker profiles

On this branch, the Signal CPU profiler also profiles its own ring-buffer reader,
report builder, and pprof encoder/uploader on Linux x86-64 and AArch64. Each worker
gets a thread CPU timer at the configured `sample_rate`, with either Python timer
mode (`ITimer` or `CTimer`). Enable it by selecting
`cpu_implementation=pyroscope.ProfilerImplementation.Signal` in `configure()`.

Native profiles are separate from the uploaded Python profiles. They are written
as uncompressed protobuf pprof files every 10 seconds and on shutdown, when there
are samples. The default directory is `./native-profiles`; set
`PYROSCOPE_NATIVE_PROFILE_DIR` to change it. Files are named `1.pprof`, `2.pprof`,
etc., skipping existing names. The `thread` label identifies each worker; values
are CPU nanoseconds.
The writer itself and reqwest's internal runtime threads are not sampled.

Framehop unwinds from the interrupted registers with its no-allocation policy.
The handler aggregates stacks in preallocated tables (4096 entries per worker,
128 frames per stack, at most 64 hash probes); protobuf serialization and filesystem
writes happen outside the handler. The dumped profiles contain addresses and ELF
mappings, with no function symbols or source lines. Contention and exhausted stack
tables produce weighted address-zero samples with a `dropped_reason` label. The
`unwind` label marks complete, failed, and depth-limited walks.

Symbolize separate copies before uploading to Pyroscope. Compile with the Cargo
release profile, retaining symbols/debug info (for example,
`CARGO_PROFILE_RELEASE_DEBUG=1 CARGO_PROFILE_RELEASE_STRIP=none cargo build --release --manifest-path rust/Cargo.toml`).
Save the exact executable and shared libraries from the profiled process under a
filesystem mirror, then run:

```bash
python3 scripts/symbolize_native_profiles.py native-profiles symbolized-profiles \
  --binary-root ./binaries --symbolizer llvm-symbolizer
```

The tool resolves ELF addresses, including DWARF lines and inline frames when
available, and preserves sample weights, labels, physical location IDs and timestamps.
Upload the files in `symbolized-profiles`; keep the raw files for later analysis.
Inspect a symbolized file with
`go tool pprof -symbolize=none -http=:8080 symbolized-profiles/1.pprof`.

This is an experiment: ELF unwind data is snapshotted at startup, without tracking
later library loads/unloads. Native reads use the same kindasafe recovery handlers and unblocked-signal contract
above, restricted to each worker's registered stack bounds. Fault recovery does
not validate unwind data or prove that arbitrary interrupted Rust borrows can be
read safely. Stop the profiler before forking, as with the signal backend itself.

## License

Apache-2.0
