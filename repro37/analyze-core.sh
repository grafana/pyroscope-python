#!/bin/sh
# Prints a backtrace for every core dump left under ./artifacts (or $1),
# using gdb from the same image the crash happened in.
DIR=${1:-artifacts}
IMAGE=${IMAGE:-pyroscope-repro37:py311}
for core in $(find "$DIR" -name 'core.*'); do
  echo "=========== $core ==========="
  docker run --rm -v "$(cd "$(dirname "$DIR")" && pwd)/$(basename "$DIR"):/cores" \
    --entrypoint gdb "$IMAGE" -batch \
    -ex "set pagination off" \
    -ex "thread apply all bt" \
    -ex "info registers" \
    /usr/local/bin/python "/cores/${core#$DIR/}"
done
