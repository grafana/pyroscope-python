#!/usr/bin/env bash
# One command from macOS: sync, build if needed, benchmark on the Linux VM,
# copy the results back.
#
# Benchmarking happens on Linux because the measurement depends on cgroup v2
# (cpu.stat and memory.peak for CPU and memory accounting) and on cpuset pinning
# via transient systemd scopes. Neither exists on macOS.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
REMOTE=${BENCH_REMOTE:-orb}
VM_SRC=${BENCH_SRC:-\$HOME/bench-src}
VM_VENV=${BENCH_VENV:-\$HOME/bench-venv}
VM_PY=$VM_VENV/bin/python

skip_build=0
entry=run_bench.py
args=()
for a in "$@"; do
    case "$a" in
        --skip-build) skip_build=1 ;;
        # Harness validation rather than a measurement: proves the guards fire
        # and that a known signal is resolvable.
        --self-check) entry=self_check.py ;;
        *) args+=("$a") ;;
    esac
done

echo "==> syncing $REPO -> $REMOTE:$VM_SRC"
BENCH_REMOTE="$REMOTE" BENCH_SRC="$VM_SRC" "$HERE/sync.sh"

if [ "$skip_build" -eq 0 ]; then
    echo "==> building the native extension in the VM"
    # Check the exit status of the build itself, not of a wrapper around it.
    ssh "$REMOTE" "cd $VM_SRC && VENV=$VM_VENV bash scripts/benchmark/bootstrap.sh $VM_SRC"
fi

echo "==> running the benchmark"
# The measurement needs the machine quiet; nothing else should run alongside it.
ssh "$REMOTE" "cd $VM_SRC/scripts/benchmark && BENCH_PYTHON=$VM_PY $VM_PY $entry ${args[*]:-}"
status=$?

echo "==> copying results back"
mkdir -p "$HERE/results"
rsync -a "$REMOTE:$VM_SRC/scripts/benchmark/results/" "$HERE/results/"
latest="$(ls -t "$HERE/results" | head -1)"
echo "==> $HERE/results/$latest"

exit $status
