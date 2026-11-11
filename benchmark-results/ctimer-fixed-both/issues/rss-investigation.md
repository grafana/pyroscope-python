Enabling CPU profiling adds **37.82–49.91 MiB of aggregate RSS under load** and **47.27–57.04 MiB while idle** in the six-worker benchmark. The implementations also differ in their resident-memory footprint. We need to identify which allocations and mappings explain this overhead, how much is common SDK initialization versus engine-specific state, and whether the retained memory is bounded.

[Full benchmark results, methodology, and reproduction instructions](https://github.com/grafana/pyroscope-squad/issues/221#issuecomment-6732569172) · [Report and reproduction ZIP](https://github.com/user-attachments/files/34564077/profiler-benchmark-report.zip)

Loaded memory (MiB):

| Profiler | Average RSS ± SD | Extra RSS vs baseline | RSS increase | Peak RSS | End PSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline (profiling disabled) | 324.03 ± 2.17 | — | — | 325.71 | 231.20 |
| py-spy | 373.94 ± 0.67 | +49.91 | +15.40% | 375.34 | 259.37 |
| Stack | 361.84 ± 0.31 | +37.82 | +11.67% | 363.23 | 257.19 |
| Signal / CTimer | 367.79 ± 1.33 | +43.76 | +13.51% | 372.99 | 262.04 |

Idle memory (MiB):

| Profiler | Average RSS ± SD | Extra RSS vs baseline | RSS increase | Peak RSS | End PSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| Baseline (profiling disabled) | 290.99 ± 1.47 | — | — | 291.94 | 198.50 |
| py-spy | 348.04 ± 2.20 | +57.04 | +19.60% | 349.59 | 224.54 |
| Stack | 343.58 ± 4.11 | +52.58 | +18.07% | 348.29 | 229.00 |
| Signal / CTimer | 338.26 ± 0.57 | +47.27 | +16.24% | 338.86 | 221.07 |

RSS and PSS are summed across the Gunicorn master and six workers. Average RSS is time-weighted within each measured window, then averaged across three repetitions; SD is between-run standard deviation. Peak RSS is the highest sampled concurrent total across the three runs. End PSS is the mean end-of-window total. RSS was sampled every 100 ms.

**Summed RSS counts shared pages in each process.** These numbers therefore do not imply the same increase in unique physical memory or private heap allocation. PSS remains higher than baseline, and its ranking differs from RSS: Signal has the highest loaded end PSS despite py-spy having the highest loaded RSS. The investigation should compare RSS, PSS, and USS at matching timestamps. The current short runs do not establish a memory leak.

Benchmark configuration:

- AMD Ryzen 9 5950X, x86_64; CPython 3.13.7, glibc 2.41, OpenSSL 3.5.7.
- SDK [PR #267](https://github.com/grafana/pyroscope-python/pull/267), revision `e60886c`; benchmark build `3638f62`.
- Django home-page rendering workload from `grafana/pyroscope` branch `python-bench`, commit `1cab57f098b7a0ece26ac98e9bcb358c52056522`; Django 5.2.8, Django REST Framework 3.16.1, Gunicorn 23.0.0.
- Fresh Docker container per measurement, six-CPU quota, CPUs 0–5, six synchronous workers. Six HTTP clients and the profile receiver run outside the application container on CPUs 6–9.
- `sample_rate=100`, `oncpu=True`, `gil_only=False`, memory profiling disabled, 10-second uploads. Signal uses CTimer; Stack uses its default per-thread CPU timer and adaptive wall sampler.
- Fifteen-second warmup followed by thirty seconds measured; three repetitions per idle/loaded case. All 24 cases completed without measured-window HTTP errors, worker failures, or CPU quota throttling.

Investigation:

1. Reproduce using the harness in the linked benchmark comment. Capture per-process RSS/PSS/USS and memory mappings before SDK import, after import, after configuration, after warmup, during steady load/idle, and after shutdown. Keep the master and individual workers separate before summing.
2. Add import-only and agent-running-with-CPU-profiling-disabled controls. The existing baseline skips SDK import/initialization, so its delta includes common library/client costs as well as the selected sampling engine.
3. Quantify file-backed/shared mappings, private anonymous allocations, resident helper-thread stacks, allocator retention, profile/ring buffers, and resolver/interning caches. Include native allocation evidence; Python allocation tracking alone will miss Rust/C++ allocations. Separate observed causes from hypotheses.
4. Repeat with one and six workers and extend run duration beyond the current short windows. Determine which costs scale per process or with profile/stack cardinality, which stabilize after warmup, and which continue growing. Measure what remains after shutdown.
5. Produce a measured breakdown that explains the RSS and PSS deltas for each implementation, identify the responsible code paths, and propose concrete reductions or a regression benchmark where appropriate.

Related: [#148](https://github.com/grafana/pyroscope-python/issues/148) documents a py-spy filename-cache growth case. It may inform the investigation, but it is not established as the cause of these results or of the overhead shared across implementations.
