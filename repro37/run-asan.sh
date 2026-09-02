#!/bin/sh
# Runs the workload against an ASAN-instrumented build of the extension
# (built from this repo's rust/ with -Zsanitizer=address).
#
#   cd rust && RUSTFLAGS="-Zsanitizer=address -Cdebuginfo=1" \
#       cargo +nightly build --release --locked --target x86_64-unknown-linux-gnu
#   cp target/x86_64-unknown-linux-gnu/release/lib_native.so \
#       repro37/asan/pyroscope/_native.so
#
# rustc only ships the static ASAN runtime, and the extension is dlopened, so
# the runtime has to be preloaded; gcc's libasan exports the same interface.
HERE=$(cd "$(dirname "$0")" && pwd)
: "${ASAN_RT:=/usr/lib/x86_64-linux-gnu/libasan.so.8}"
export LD_PRELOAD="$ASAN_RT"
export ASAN_OPTIONS="verify_asan_link_order=0:detect_leaks=0:halt_on_error=0:log_path=${ASAN_LOG:-/tmp/asan}:print_stacktrace=1"
export PYTHONMALLOC=malloc
export PYTHONPATH="$HERE/asan:$HERE"
exec "$HERE/.venv/bin/python" "$HERE/workload.py" "$@"
