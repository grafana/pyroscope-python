#!/usr/bin/env bash
set -euo pipefail
# Run from the profiler repository at the commit recorded in metadata.json.
# Use a fresh output directory; earlier results must never be appended to a rerun.
task_dir=$(mktemp -d /tmp/python-profiler-bench-XXXXXX)
results_dir=$(realpath benchmark-results/python-bench)
test "$(git rev-parse HEAD)" = f6a0e3e25d91a4d7439f4027d11ee5aaf7f84956
git clone --depth 1 --single-branch --branch python-bench \
  git@github.com:grafana/pyroscope.git "$task_dir/pyroscope"
git -C "$task_dir/pyroscope" fetch --depth 1 origin 1cab57f098b7a0ece26ac98e9bcb358c52056522
git -C "$task_dir/pyroscope" switch --detach 1cab57f098b7a0ece26ac98e9bcb358c52056522
git -C "$task_dir/pyroscope" apply "$results_dir/benchmark.patch"
git submodule update --init --recursive
uv venv --python /home/korniltsev/.pyenv/versions/3.13.7/bin/python3.13 "$task_dir/venv"
RUSTUP_TOOLCHAIN=1.99.1 CC=clang CXX=clang++ \
  CFLAGS=--gcc-install-dir=/usr/lib/gcc/x86_64-redhat-linux/16 \
  CXXFLAGS=--gcc-install-dir=/usr/lib/gcc/x86_64-redhat-linux/16 \
  LIBRARY_PATH=/usr/lib/gcc/x86_64-redhat-linux/16 \
  uv pip install --python "$task_dir/venv/bin/python" . \
    'Django==5.2.8' 'djangorestframework==3.16.1' 'gunicorn==23.0.0' \
    'asgiref==3.13.0' 'packaging==26.3' 'psutil==7.2.2' 'sqlparse==0.6.1'
go build -o "$results_dir/loadgen" "$results_dir/loadgen.go"
"$task_dir/venv/bin/python" "$results_dir/run_benchmark.py" \
  --app "$task_dir/pyroscope/examples/language-sdk-instrumentation/python/rideshare/django/app" \
  --output "$results_dir/rerun-$(date +%Y%m%d-%H%M%S)" \
  --duration 30 --warmup 5 --repeats 3
