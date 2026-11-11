#!/usr/bin/env bash
set -euo pipefail
# The pinned source context and built image are retained on this host.
# Results from each rerun are kept in a new directory.
ssh user@5950x-vm-vibe.tail437fc2.ts.net '
  set -eu
  cd /home/user/pyroscope-python-bench-r7CPmW
  sudo -n python3 run_remote.py --output "rerun-$(date +%Y%m%d-%H%M%S)"
'
