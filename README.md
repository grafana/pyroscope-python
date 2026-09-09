# pyroscope-python

Pyroscope continuous profiling agent for Python applications.

Uses [py-spy](https://github.com/benfred/py-spy) by default, with an optional
Google Cloud Profiler CPU sampler, and sends profiles to a Pyroscope server.

## Installation

```bash
pip install pyroscope-io
```

## GCP CPU profiler

The default CPU profiler remains `pyspy`. On Linux with a GIL-enabled CPython
3.10 or 3.11 build, the native CPU sampler from Google Cloud Profiler can be
selected:

```python
import pyroscope

pyroscope.configure(
    application_name="my.service",
    cpu_profiler="gcp",
)
```

The GCP backend is CPU-only, uses the same default sampling rate of 100 Hz
(10 ms), and does not implement wall profiling or GIL-only sampling. Set
`oncpu=True` (the default). The py-spy-specific `gil_only`, `line_no`,
`report_thread_id`, and `report_thread_name` options, as well as dynamic
thread tags, do not affect GCP samples. `report_pid` remains supported.

## License

Apache-2.0
