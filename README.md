# pyroscope-python

Pyroscope continuous profiling agent for Python applications.

Uses [py-spy](https://github.com/benfred/py-spy) for stack sampling and the [pyroscope](https://crates.io/crates/pyroscope) Rust crate to send profiles to a Pyroscope server.

## Installation

```bash
pip install pyroscope-io
```

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

## License

Apache-2.0
