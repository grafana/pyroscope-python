# Python profiler benchmark report

AMD Ryzen 9 5950X (x86_64), CPython 3.13.7, six allocated CPUs and six workers. Each case uses a fresh Docker container, a 15-second warmup and a 30-second measurement, repeated three times. All profilers use `gil_only=False`; Signal uses CTimer at 100 Hz.

Stack recorded the lowest mean loaded CPU cost and RSS among the profilers. Signal / CTimer recorded the lowest idle CPU use and idle RSS. Mean throughput differences from the unprofiled baseline were below 1%.

Loaded workload:

| Profiler | Requests/s | CPU cost vs baseline | Average RSS (MiB) | Peak RSS (MiB) |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 10,490.2 | — | 324.03 | 325.71 |
| py-spy | 10,394.3 | +5.25% | 373.94 | 375.34 |
| Stack | 10,535.0 | +0.48% | 361.84 | 363.23 |
| Signal / CTimer | 10,434.2 | +1.02% | 367.79 | 372.99 |

Idle costs and loaded CPU-weight accounting:

| Profiler | Idle CPU seconds per 30 s | Idle average RSS (MiB) | Loaded uploaded weight / worker CPU |
| --- | ---: | ---: | ---: |
| Baseline | 0.007 | 290.99 | — |
| py-spy | 4.673 | 348.04 | 81.42–81.46% |
| Stack | 0.360 | 343.58 | 98.89–98.94% |
| Signal / CTimer | 0.203 | 338.26 | 99.58–99.63% |

Values are means of three repetitions; peak RSS is the highest observed sample. All 24 runs passed. Small performance differences should be read alongside the variation below. Aggregate CPU-weight accounting and stack-attribution accuracy are separate measurements; this benchmark does not establish attribution accuracy.

<details>
<summary>Full report, methodology, and reproduction instructions</summary>

This benchmark compares py-spy, Stack and Signal / CTimer with an unprofiled baseline. Under load, mean CPU cost per request was 5.25% above baseline for py-spy, 0.48% for Stack, and 1.02% for Signal / CTimer. Stack had the lowest loaded RSS among the profilers; Signal / CTimer had the lowest idle CPU use and idle RSS.

All implementations use the same SDK build, application and runtime configuration. Smoke tests and diagnostic experiments are excluded from the benchmark statistics. CPU, memory, throughput and aggregate profile weight are reported separately; these measurements do not establish stack-attribution accuracy.

## Configuration

| Setting | Value |
| --- | --- |
| SDK revision | [PR #267](https://github.com/grafana/pyroscope-python/pull/267), revision `e60886c`; benchmark build `3638f62` |
| Hardware | AMD Ryzen 9 5950X 16-Core Processor, x86_64 |
| Benchmark CPU allocation | Six CPUs, pinned to CPUs 0–5 |
| Kernel | Linux 7.0.0-42-generic |
| Runtime | CPython 3.13.7, built with GCC 14.2.0; glibc 2.41 |
| TLS library | OpenSSL 3.5.7 |
| Application | Django home-page rendering workload from the `python-bench` branch |
| Dependencies | Django 5.2.8, Django REST Framework 3.16.1, Gunicorn 23.0.0 |
| Isolation | Fresh Docker container per measurement, with a six-CPU quota |
| Concurrency | Six synchronous Gunicorn workers and six closed-loop HTTP clients |
| Client and profile receiver | Outside the application container, pinned to CPUs 6–9 |
| Common profiler settings | 100 Hz setting, `oncpu=True`, `gil_only=False`, memory profiling disabled, 10-second uploads |
| Signal configuration | CTimer |
| Stack configuration | Default per-thread 10 ms CPU timer and adaptive wall sampler |
| Warmup and measurement | 15-second warmup after all workers initialize, then 30 seconds measured |
| Repetitions | Three per implementation, idle and loaded: 24 runs |
| Order | Sequential cases, shuffled within each repetition with seed `20261109` |
| Run started | 2026-11-09 03:45:20 UTC |

The old benchmark dependency pins were updated for CPython 3.13 compatibility. The application workload was preserved, and all cases used the same image.

## Loaded CPU and throughput

Values are arithmetic means across three repetitions. `±` denotes the between-run sample standard deviation, not a confidence interval. CPU cost per request is (master + worker user/system CPU time) / successful measured requests. Deltas compare each implementation mean with the baseline mean.

| Profiler | Requests/s ± SD | Throughput delta | CPU µs/request ± SD | CPU-cost delta |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 10490.2 ± 23.1 | +0.00% | 439.87 ± 2.26 | +0.00% |
| py-spy | 10394.3 ± 21.7 | -0.91% | 462.96 ± 1.59 | +5.25% |
| Stack | 10535.0 ± 54.0 | +0.43% | 441.96 ± 2.26 | +0.48% |
| Signal / CTimer | 10434.2 ± 9.3 | -0.53% | 444.34 ± 0.97 | +1.02% |

All mean throughput differences were below 1%. Three repetitions do not establish a reliable throughput ranking or significance for the smaller CPU-cost differences. In particular, the positive Stack throughput delta should not be interpreted as a demonstrated speedup.

## Loaded CPU time

CPU seconds consumed during each 30-second measurement, averaged across runs. The totals include profiler threads inside workers; the external load generator, receiver and in-container measurement process are excluded.

| Profiler | User CPU (s) | System CPU (s) | Total CPU (s) | Average cores |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 34.490 | 103.943 | 138.433 | 4.6143 |
| py-spy | 35.507 | 108.860 | 144.367 | 4.8121 |
| Stack | 36.010 | 103.673 | 139.683 | 4.6560 |
| Signal / CTimer | 32.383 | 106.710 | 139.093 | 4.6363 |

## Loaded memory

All memory values are MiB, summed across the master and six workers. Average RSS is time-weighted within each run. Peak is the highest sampled concurrent total across all three runs. PSS is the mean end-of-run total.

| Profiler | Average RSS ± SD | Extra RSS | RSS increase | Peak RSS | End PSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline | 324.03 ± 2.17 | +0.00 | +0.00% | 325.71 | 231.20 |
| py-spy | 373.94 ± 0.67 | +49.91 | +15.40% | 375.34 | 259.37 |
| Stack | 361.84 ± 0.31 | +37.82 | +11.67% | 363.23 | 257.19 |
| Signal / CTimer | 367.79 ± 1.33 | +43.76 | +13.51% | 372.99 | 262.04 |

## Idle memory

All memory values are MiB, summed across the master and six workers. Average RSS is time-weighted within each run. Peak is the highest sampled concurrent total across all three runs. PSS is the mean end-of-run total.

| Profiler | Average RSS ± SD | Extra RSS | RSS increase | Peak RSS | End PSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline | 290.99 ± 1.47 | +0.00 | +0.00% | 291.94 | 198.50 |
| py-spy | 348.04 ± 2.20 | +57.04 | +19.60% | 349.59 | 224.54 |
| Stack | 343.58 ± 4.11 | +52.58 | +18.07% | 348.29 | 229.00 |
| Signal / CTimer | 338.26 ± 0.57 | +47.27 | +16.24% | 338.86 | 221.07 |

RSS was sampled every 100 ms, so shorter spikes may be missed. Summed RSS counts shared pages in each process; PSS apportions shared pages and is provided as a complementary measure.

## Idle CPU

CPU seconds per 30-second window, averaged across three repetitions.

| Profiler | User CPU (s) | System CPU (s) | Total CPU (s) ± SD | Average cores |
| --- | ---: | ---: | ---: | ---: |
| Baseline | 0.007 | 0.000 | 0.007 ± 0.006 | 0.0002 |
| py-spy | 0.000 | 4.673 | 4.673 ± 0.035 | 0.1558 |
| Stack | 0.253 | 0.107 | 0.360 ± 0.010 | 0.0120 |
| Signal / CTimer | 0.140 | 0.063 | 0.203 ± 0.012 | 0.0068 |

## Uploaded CPU weight under load

These totals cover the complete profiler lifetime, including startup, warmup and shutdown; they must not be compared directly with only the 30-second measurement-window CPU totals above.

For each worker, `resource.getrusage(RUSAGE_SELF)` was recorded around configure and shutdown. Summed lifecycle bounds are:

```text
lower = CPU before shutdown - CPU after configure
upper = CPU after shutdown - CPU before configure
```

The reported ratio range is uploaded weight / upper through uploaded weight / lower.

| Profiler | Uploaded CPU (s) | Observed worker CPU bounds (s) | Weight / worker CPU |
| --- | ---: | ---: | ---: |
| py-spy | 177.460 | 217.850–217.966 | 81.42–81.46% |
| Stack | 208.578 | 210.803–210.921 | 98.89–98.94% |
| Signal / CTimer | 209.370 | 210.139–210.249 | 99.58–99.63% |

The denominator includes every worker thread, while engines may exclude some native threads or apply sampling filters. These ratios compare accounting; they are not universal measures of profile accuracy.

## Validation and limitations

- All 24 measurements passed; 3,766,920 successful measured requests.
- Zero measured-window HTTP errors, detected Rust panics, worker replacements or unclean process exits. All 18 profiled cases produced nonempty profiles.
- CPU quota, CPU affinity, six workers, profiler settings, workload hash and the recorded native SDK binary were verified for every case.
- No CPU quota throttling was recorded.
- A local HTTP sink acknowledged and saved uploads. Profile encoding and transmission remained enabled; backend ingestion costs were not measured.
- CPU affinity and quota do not guarantee exclusive use of physical cores. The results apply to this workload, hardware and runtime configuration.
- Sampling mechanisms differ. Lower overhead does not establish equivalent stack coverage or attribution accuracy.

## Individual loaded measurements

| Repeat | Profiler | Requests/s | CPU µs/request | Average RSS MiB | Peak RSS MiB |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | Baseline | 10472.3 | 442.04 | 321.52 | 321.93 |
| 1 | py-spy | 10396.7 | 462.86 | 373.85 | 374.51 |
| 1 | Stack | 10579.5 | 441.00 | 361.66 | 362.29 |
| 1 | Signal / CTimer | 10430.3 | 444.24 | 366.62 | 370.09 |
| 2 | Baseline | 10516.3 | 437.53 | 325.19 | 325.57 |
| 2 | py-spy | 10371.5 | 464.60 | 374.65 | 375.34 |
| 2 | Stack | 10474.9 | 444.54 | 362.21 | 363.23 |
| 2 | Signal / CTimer | 10427.4 | 445.36 | 369.23 | 372.99 |
| 3 | Baseline | 10482.0 | 440.04 | 325.37 | 325.71 |
| 3 | py-spy | 10414.7 | 461.42 | 373.32 | 374.21 |
| 3 | Stack | 10550.5 | 440.35 | 361.67 | 362.55 |
| 3 | Signal / CTimer | 10444.8 | 443.43 | 367.52 | 370.70 |

## Reproducibility

- **Tested SDK commit:** `3638f627472fd4f326c435b1c493a3d1227d81b7`
- **PR revision:** [#267](https://github.com/grafana/pyroscope-python/pull/267) at `e60886ce78175c54732ac19e2d7283ae018c9f5b`
- **Benchmark commit:** `1cab57f098b7a0ece26ac98e9bcb358c52056522`
- **Docker image:** `sha256:2162c38250a33a20825b63bc6ff5790e4125b37845c1b82d168c97c2ceb7b932`
- **Wheel SHA256:** `a65d166736293e81f70fa4dafff517803e0892b6ade31e8df73fad9ea86a57ce`
- **Native SDK SHA256:** `a3ed49cb73f376658cf62e4e5238eef63e80ff70b44a60352e2928ccbb8e76df`

## Reproduce this benchmark

The accompanying ZIP contains this report and `benchmark-reproduction.tar.gz`. Extract the ZIP first. The reproduction bundle contains the exact SDK wheel used for this report, the application, Docker build context, load generator, measurement scripts and analysis scripts. The benchmark harness is supplied separately from the upstream SDK repository.

Requirements:

- Linux x86_64 with Docker and cgroup v2, host Python 3, and permission to run Docker through `sudo`.
- At least ten available logical CPUs: application CPUs `0–5`, client/receiver CPUs `6–9`.
- Free local TCP ports `18080`, `18081` and `18082`.
- Internet access to pull the pinned Python base image and install the pinned Python dependencies and required system libraries.
- Run other CPU-heavy work separately. Match the recorded hardware, kernel and runtime versions when comparing absolute results.

### 1. Unpack and verify the inputs

Save the bundle in your working directory, then run these commands in Bash:

```bash
set -euo pipefail

printf '%s  %s\n' \
  'a1de4511e5c42a931a8a49b33263e2df50b1656f51245f79c73554b32a16fe88' \
  'benchmark-reproduction.tar.gz' | sha256sum --check -

tar -xzf benchmark-reproduction.tar.gz
cd benchmark-reproduction
tar -xzf runtime-context.tar.gz

printf '%s  %s\n' \
  'a65d166736293e81f70fa4dafff517803e0892b6ade31e8df73fad9ea86a57ce' \
  'runtime/wheels/pyroscope_io-1.4.0-cp313-cp313-linux_x86_64.whl' | sha256sum --check -

chmod +x loadgen
```

The supplied load generator is the binary used in the recorded runs. Its source, `loadgen.go`, is also included. The SDK wheel and source commit identifiers are recorded in `source.json`.

### 2. Build the application image

```bash
sudo docker build \
  --tag pyroscope-python-bench:benchmark-3638f62 \
  runtime
```

The Dockerfile pins the Python base-image digest and Python package versions. System-library packages are obtained from the configured Debian repositories; compare the captured libc/OpenSSL versions with this report if reproducing later. Reusing the recorded image, when available, also preserves those library versions.

### 3. Run the smoke check and measured suite

Use a fresh output directory for every attempt:

```bash
run_dir=$(mktemp -d "$PWD/benchmark-results-XXXXXX")
printf 'Results directory: %s\n' "$run_dir"

sudo python3 run_benchmark.py \
  --image pyroscope-python-bench:benchmark-3638f62 \
  --cpuset 0-5 --aux-cpus 6-9 \
  --smoke --output "$run_dir/smoke" \
  > "$run_dir/smoke.log" 2>&1

sudo python3 run_benchmark.py \
  --image pyroscope-python-bench:benchmark-3638f62 \
  --cpuset 0-5 --aux-cpus 6-9 \
  --output "$run_dir/measured" \
  > "$run_dir/run.log" 2>&1
```

The measured command runs all 24 cases sequentially: baseline, py-spy, Stack and Signal/CTimer; idle and loaded; three repetitions each. It takes approximately 18 minutes plus startup and shutdown time. The runner creates a fresh six-worker container for every case, uses a six-CPU quota, keeps the six-client load generator outside it, and removes each container after completion.

The mounted `wsgi.py` explicitly sets `sample_rate=100`, `oncpu=True`, `gil_only=False`, and `signal_timer=CTimer`. Measurements include CPU time, successful requests, RSS sampled every 100 ms, end-of-window PSS, and worker CPU counters around profiler startup/shutdown. Profile uploads are saved by a local HTTP receiver.

Watch `run.log` from another terminal for `START` records and the final `COMPLETE 24`. Smoke results are kept separately and excluded from the measured statistics.

### 4. Decode, summarize and validate

After the measured command completes:

```bash
sudo python3 decode_profiles.py "$run_dir/measured" > "$run_dir/decode.log"
sudo python3 summarize.py "$run_dir/measured"
sudo python3 coverage.py "$run_dir/measured"
sudo python3 validate.py "$run_dir/measured"
```

The measured output directory contains `comparison.txt` and `summary.csv` for the performance results, `coverage.txt` for lifecycle CPU accounting, and `validation.json` for the checks. Individual measurements, RSS timelines, container settings, logs and complete profile stacks remain in the same directory.

Validation checks all 24 runs, six-CPU limits, six workers, profiler settings, the workload and native SDK hashes, successful shutdown, worker stability and nonempty profiles. Reproduction can yield different timings and sample distributions; compare totals and full stack proportions separately rather than treating matching totals as proof of accurate attribution.

</details>
