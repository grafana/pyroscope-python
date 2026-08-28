#!/usr/bin/env bash
# Push the working tree to the benchmark host.
#
# Host and destination come from the environment so the same harness can target
# any machine with cgroup v2 and systemd:
#
#   BENCH_REMOTE   ssh destination           (default: orb)
#   BENCH_SRC      path on that host         (default: ~/bench-src)
#
# On the OrbStack VM the macOS tree is visible at the same path, so the copy is
# local; to any other host it goes over ssh.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REMOTE="${BENCH_REMOTE:-orb}"
SRC="${BENCH_SRC:-\$HOME/bench-src}"

# Refuse to replace the harness underneath a running benchmark. Every measured
# run launches a fresh subprocess from this tree, so a mid-run sync would leave
# later cells of the matrix measured by different code than earlier ones, and
# nothing downstream would reveal that it happened. Pass --force to override.
if [ "${1:-}" != "--force" ] \
   && ssh "$REMOTE" 'pgrep -f "run_bench[.]py|fullrun[.]sh" >/dev/null'; then
    echo "refusing to sync: a benchmark is running on $REMOTE" >&2
    echo "wait for it to finish, or pass --force if you are sure" >&2
    exit 1
fi

EXCLUDES=(
    --exclude '.venv' --exclude 'build' --exclude 'rust/target'
    --exclude '.git' --exclude 'scripts/benchmark/results'
    --exclude '__pycache__'
)

if ssh "$REMOTE" "test -d '$REPO'" 2>/dev/null; then
    # Source tree already visible on the host: copy locally there, no transfer.
    ssh "$REMOTE" "mkdir -p '$SRC' && rsync -a --delete ${EXCLUDES[*]} '$REPO/' '$SRC/'"
else
    ssh "$REMOTE" "mkdir -p '$SRC'"
    rsync -a --delete "${EXCLUDES[@]}" -e ssh "$REPO/" "$REMOTE:$SRC/"
fi
