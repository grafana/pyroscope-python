Corrected remote benchmark: all threads and Signal CTimer

All profiler configure() calls explicitly use gil_only=False, oncpu=True,
sample_rate=100 and signal_timer=CTimer. Stack keeps its default per-thread
CPU timer; py-spy uses its own polling sampler. No ITimer benchmark cases run.

Each measurement gets a fresh Docker container limited to 6 CPUs and guest
CPUs 0-5, with 6 workers; clients and receiver use guest CPUs 6-9.
Three repetitions, 15s warmup and 30s measurement for idle and loaded cases.
RSS is sampled every100ms across master plus workers.

The application WSGI file is mounted over the pinned image, and its SHA256
and each worker configuration are checked. Runtime CPU bounds bracket
configure()/shutdown() and cover the full profiler lifetime.

Source commits are recorded in source.json; the SDK image is unchanged.
Remote directory: /home/user/pyroscope-python-bench-r7CPmW/all-threads-ctimer
Remote host: user@5950x-vm-vibe.tail437fc2.ts.net

Reproduce there with: sudo python3 run_remote.py --output NEW_DIRECTORY
Analyze: sudo python3 decode_profiles.py NEW_DIRECTORY
         sudo python3 summarize.py NEW_DIRECTORY
         sudo python3 coverage.py NEW_DIRECTORY

Old gil_only=True/ITimer results are in the parent directory and excluded.
The first smoke attempt hit an interleaved JSON logging error; single-write
records fixed it, and a fresh four-case smoke passed before measurement.
