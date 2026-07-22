# pyroscope-python

Pyroscope continuous profiling agent for Python applications.

Uses [py-spy](https://github.com/benfred/py-spy) for stack sampling and the [pyroscope](https://crates.io/crates/pyroscope) Rust crate to send profiles to a Pyroscope server.

## Installation

```bash
pip install pyroscope-io
```

## Memory-only profiling

CPU profiling is enabled by default. To collect memory profiles without
starting the py-spy CPU sampler, disable it explicitly:

```python
import pyroscope

pyroscope.configure(
    application_name="my-service",
    server_address="http://localhost:4040",
    cpu_enabled=False,
    mem_enabled=True,
)
```

## License

Apache-2.0
