#!/usr/bin/env bash
# Sync this repo into the Linux VM and build the native extension there.
# Compiling on the virtiofs mount is slow and rust/target already holds a macOS
# build, so everything happens in VM-local directories.
set -euo pipefail

SRC="${1:-$HOME/bench-src}"
VENV="${VENV:-$HOME/bench-venv}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/pyroscope-bench-target}"
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

cd "$SRC"

if [ ! -d "$VENV" ]; then
    # Any supported interpreter; pinning a minor version would fail on a host
    # that ships a different one, and the extension is built against whichever
    # interpreter is used here.
    uv venv "$VENV"
fi

uv pip install --python "$VENV" -q setuptools 'setuptools-rust>=1.12.0,<2.0.0'
uv pip install --python "$VENV" -q --no-build-isolation --force-reinstall --no-deps .
uv pip install --python "$VENV" -q -r scripts/benchmark/requirements.txt

"$VENV/bin/python" -c 'import pyroscope; print("pyroscope ok:", pyroscope._native.__file__)'
