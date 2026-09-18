# pyroscope-python

Pyroscope continuous profiling agent for Python applications.

Uses [py-spy](https://github.com/benfred/py-spy) for stack sampling and the [pyroscope](https://crates.io/crates/pyroscope) Rust crate to send profiles to a Pyroscope server.

## Installation

```bash
pip install pyroscope-io
```

## Memory profiling

Memory profiling is compiled into every build; there is no optional Cargo
`memory` feature. Enable it with `pyroscope.configure(mem_enabled=True, ...)`.

On free-threaded CPython, memory profiling is unsupported even when the GIL is
enabled. Requesting it emits a Python `RuntimeWarning` (including when logging is
disabled) and disables memory profiling for that configuration. CPU profiling
remains configured if enabled. If neither profiler remains enabled, `configure()`
returns `False` without starting an agent. Treating the warning as an error raises
before the agent starts. The pinned PyO3 dependency does not support building for
free-threaded Python 3.13.

## License

Apache-2.0
