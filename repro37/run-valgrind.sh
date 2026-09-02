#!/bin/sh
set -e
python /app/sink.py 4040 &
sleep 1
exec valgrind \
  --tool=memcheck \
  --error-exitcode=99 \
  --num-callers=25 \
  --track-origins=yes \
  --errors-for-leak-kinds=none \
  --leak-check=no \
  python /app/workload.py "$@"
