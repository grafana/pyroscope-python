# Vendored Google Cloud Profiler native sources

## Baseline and provenance

The copied sources under `gcp/` come from
`GoogleCloudPlatform/cloud-profiler-python` release `v4.1.0`, commit
`23e3c4845b6fc6aa07b362ffe8f1be74630cc0d2`.

## File inventory

Copied from `googlecloudprofiler/src/`:

- `gcp/profiler.h`
- `gcp/profiler.cc`
- `gcp/stacktraces.h`
- `gcp/stacktraces.cc`
- `gcp/populate_frames.h`
- `gcp/populate_frames.cc`
- `gcp/clock.h`
- `gcp/clock.cc`
- `gcp/log.h`
- `gcp/log.cc`

Copied unchanged from the repository root:

- `gcp/LICENSE`

Intentionally omitted:

- `googlecloudprofiler/src/_profiler.cc`, the upstream Python extension wrapper
- all non-native Python client and packaging files
- wall-profiler code; none is copied or introduced

## Patch 1: reject free-threaded CPython

File: `gcp/profiler.h`

The vendored collector relies on the GIL for code-object lifetime tracking and
profile materialization. A compile-time `Py_GIL_DISABLED` guard prevents an
unsupported free-threaded build. The file carries a Grafana modification
notice pointing to this document.

No Python-version compatibility code has been added. The collector retains
upstream's CPython support and implementation unchanged.

## Behavior retained

Collection remains synchronous: `CPUProfiler::Collect()` releases the GIL,
samples with `ITIMER_PROF`/`SIGPROF`, then reacquires the GIL and returns the
Python dictionary. Frame capture and output remain leaf-first. No wall
profiler or GIL-only mode is included.

## Apache 2.0 notice

The vendored Google files are Copyright 2018 Google LLC and are licensed under
the Apache License, Version 2.0. The full upstream license is preserved at
`gcp/LICENSE`. Upstream source license headers are retained.
