Investigate why **Signal / CTimer adds 1.02% CPU overhead per request above the unprofiled baseline, while Stack adds 0.48%** in the same loaded workload.

This corresponds to approximately **4.47 µs/request of additional CPU for Signal versus 2.09 µs/request for Stack**: about 2.1× the incremental overhead. The question is which Signal code paths account for the extra **2.38 µs/request**, and whether that work can be reduced while preserving profiling coverage.

[Full benchmark results, methodology, and reproduction instructions](https://github.com/grafana/pyroscope-squad/issues/221#issuecomment-6732569172) · [Report and reproduction ZIP](https://github.com/user-attachments/files/34564077/profiler-benchmark-report.zip)

Loaded results:

| Profiler | CPU µs/request ± SD | Extra CPU µs/request vs baseline | CPU overhead vs baseline | Requests/s ± SD |
| --- | ---: | ---: | ---: | ---: |
| Baseline (profiling disabled) | 439.87 ± 2.26 | — | — | 10,490.2 ± 23.1 |
| Stack | 441.96 ± 2.26 | +2.09 | +0.48% | 10,535.0 ± 54.0 |
| Signal / CTimer | 444.34 ± 0.97 | +4.47 | +1.02% | 10,434.2 ± 9.3 |

Values are means of three repetitions; SD is between-run sample standard deviation. CPU cost is total master/worker user-plus-system CPU divided by successful requests, including profiler threads but excluding the measurement process, load generator and receiver. The percentage overheads are relative to the same unprofiled baseline. More repetitions are needed to establish the precision of these small overhead estimates.

Configuration: AMD Ryzen 9 5950X, x86_64, CPython 3.13.7; fresh Docker container per case, six-CPU quota and six workers; six HTTP clients outside the application container. Fifteen seconds warmup, then thirty seconds measured. Same SDK build ([PR #267](https://github.com/grafana/pyroscope-python/pull/267), revision `e60886c`; benchmark build `3638f62`), `sample_rate=100`, `oncpu=True`, `gil_only=False`, memory profiling disabled, and 10-second uploads. Signal uses CTimer; Stack uses its default per-thread CPU timer and adaptive wall sampler. No CPU quota throttling was recorded.

Investigation should:

- Reproduce the loaded comparison with more repetitions and longer windows, retaining the unprofiled baseline so incremental CPU costs remain comparable.
- Attribute Signal's additional CPU relative to Stack across signal-handler capture, collection/draining, deferred frame resolution, allocation and locking, aggregation, and encoding/upload. Separate common SDK costs from implementation-specific work.
- Measure actual callback/stack counts, represented sampling periods, dropped weights and uploaded CPU weight alongside CPU cost. Compare equivalent profiling work rather than assuming a nominal 100 Hz setting produces equivalent capture rates.
- Identify concrete avoidable work in Signal, or explain why the additional work is necessary. Any proposed optimization should retain profile weight and stack coverage rather than lower the sampling rate.

Expected outcome: a measured explanation for the **1.02% versus 0.48% loaded CPU overhead**, with responsible Signal code paths and actionable optimization or regression-benchmark follow-ups.

Related memory investigation: [#269](https://github.com/grafana/pyroscope-python/issues/269).
