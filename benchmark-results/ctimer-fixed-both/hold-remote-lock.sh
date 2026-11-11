#!/usr/bin/env bash
set -euo pipefail
exec 9>/tmp/pyroscope-cpu-loss-experiments.lock
flock -n 9
echo benchmark-lock-acquired
while test -e /home/user/pyroscope-ctimer-fixed-3638f62/ACTIVE; do
  sleep 2
done
