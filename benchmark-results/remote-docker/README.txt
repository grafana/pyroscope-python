Remote Docker benchmark artifacts

Remote working directory:
  user@5950x-vm-vibe.tail437fc2.ts.net:/home/user/pyroscope-python-bench-r7CPmW

The context/ directory on the remote contains git-archive exports of profiler
main f6a0e3e25d91a4d7439f4027d11ee5aaf7f84956 and its pinned submodules, plus
the Django app from benchmark branch commit 1cab57f098b7a0ece26ac98e9bcb358c52056522.
benchmark.patch records the instrumentation changes to the benchmark application.
No profiler source changes were made.

Dockerfile builds the release SDK and pins application dependencies. The actual
image ID and parent image metadata are retained in measured/host-metadata.json.
The source image contains source.json; measure.py is mounted from the host harness.

Rebuild on the remote:
  cd /home/user/pyroscope-python-bench-r7CPmW
  sudo docker build -t pyroscope-python-bench:f6a0e3e context

Run on the remote (use a fresh directory):
  sudo python3 run_remote.py --output rerun-YYYYMMDD-HHMMSS

Analyze completed runs on the remote:
  sudo python3 summarize.py measured
  sudo python3 decode_profiles.py measured

Load generator source is loadgen.go; build with:
  CGO_ENABLED=0 go build -o loadgen loadgen.go

Each run starts one fresh container with a quota of 6 CPUs and affinity to guest
CPUs 0-5, with 6 Gunicorn workers. The controller, receiver and six-client Go load
generator run outside that container on guest CPUs 6-9. RSS sampling is every
100 ms. RSS includes the Gunicorn master and workers, but not the controller or
load generator. Average RSS is time-weighted. Peak RSS is the highest sampled
concurrent total. PSS is also recorded to account for shared pages.

All benchmark containers are stopped and removed after their case. The Docker
installation, image, source context, harness and result files are retained.

Local laptop results are separate in ../python-bench/ and are not included in
this remote comparison. Remote smoke runs are separate from measured/.
