# CPU profiler overhead benchmark

This suite compares a process that never imports Pyroscope (`none`) with the
current py-spy CPU profiler (`current`). It is intended for a quiet Linux host
with Docker and cgroup v2; it does not define pass/fail performance thresholds.

## Workloads

| Name | Shape |
|---|---|
| `cpu-1` | Pure-Python integer work on one thread |
| `cpu-4` | The same work on four threads |
| `io-1` | One thread doing delayed HTTP round trips to an external sink |
| `io-4` | Four threads doing those round trips |
| `http-cpu` | Four Gunicorn sync workers serving a Python-heavy endpoint |
| `http-io` | Four Gunicorn sync workers serving a blocking-I/O endpoint |

Every active workload path retains at least 128 controlled Python stack frames,
including each Gunicorn request handler. The configured depth is recorded in
`metadata.json` and the generated report. The collector decodes the uploaded
pprof payloads, counts each sample's location IDs, and fails a profiled trial
unless at least one uploaded sample contains the full configured depth.

The Gunicorn application is not preloaded, so each profiled worker initializes
Pyroscope after `fork()`. Profiles and standalone I/O requests go to separate
service containers on dedicated CPUs. Their CPU and memory, and those of the
HTTP load generator, are not charged to the workload container.

## Run

From the repository root on the Linux benchmark host:

```sh
make benchmark/overhead/build
make benchmark/overhead/test
make benchmark/overhead/smoke
make benchmark/overhead/run
```

The full run defaults to seven 20-second repetitions. Target containers are
limited to CPUs `0-3` and 2 GiB. HTTP load generation uses CPUs `4-7`, profile
collection uses `8-9`, and the standalone I/O sink uses `10-11`. Override
settings directly when needed:

```sh
python3 perf/profiler_overhead/run.py \
  --duration 30 \
  --repetitions 10 \
  --target-cpus 4-7 \
  --load-cpus 8-11 \
  --collector-cpus 12-13 \
  --io-cpus 14-15 \
  --memory 2g
```

Use `--workloads cpu-1,io-4,http-cpu` to run a subset. All four CPU sets must be
available and non-overlapping.

## Results

Each run creates a timestamped, ignored directory under `results/` containing:

- `raw.jsonl`: one record per workload, mode, and repetition.
- `metadata.json`: host, image, dependency, and run settings.
- `report.md`: paired medians and 95% bootstrap confidence intervals.

CPU usage comes from `cpu.stat`. Mean and peak container memory are sampled
from `memory.current` every 50 ms. Throughput is measured over a fixed-duration
window, so CPU-bound profiler cost primarily appears as reduced throughput;
CPU cost per 1,000 operations remains comparable across all workload types.

Trials are paired by repetition, and mode order alternates to reduce drift.
Run on an otherwise idle host and retain the raw output when comparing changes.

## Reproducibility and extension

The image pins the amd64 Python 3.13 and Rust 1.96 image manifests, Python build
tools, and Gunicorn. Debian build packages still come from the Bookworm
repository, so `metadata.json` records the resulting image ID and installed
runtime versions for exact identification of a run.

Profiler selection is isolated in `bench/profilers.py`. To benchmark another
implementation, add a mode that returns a shutdown callback; workloads,
container accounting, and reports require no changes.

