#!/usr/bin/env bash
# Build an instrumented interpreter and SDK, then use the native integration harness.
set -euo pipefail

repo_dir=$(cd "$(dirname "$0")/.." && pwd)
python_ref=${ASAN_PYTHON_REF:-3.14}
asan_dir=${ASAN_BUILD_DIR:-"$repo_dir/.cache/asan/$python_ref"}
mkdir -p "$asan_dir"
asan_dir=$(cd "$asan_dir" && pwd)
export CC=${CC:-clang-18}
export CXX=${CXX:-clang++-18}
export ASAN_SYMBOLIZER_PATH=${ASAN_SYMBOLIZER_PATH:-/usr/bin/llvm-symbolizer-18}
export ASAN_OPTIONS=detect_leaks=0
export LD_LIBRARY_PATH="$asan_dir/install/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

if [ ! -x "$asan_dir/install/bin/python3" ]; then
    if [ ! -d "$asan_dir/cpython" ]; then
        git clone --depth 1 --branch "$python_ref" https://github.com/python/cpython.git "$asan_dir/cpython"
    fi
    (
        cd "$asan_dir/cpython"
        ./configure --prefix="$asan_dir/install" --enable-shared --with-address-sanitizer --without-pymalloc --with-ensurepip=install
        make -j"$(getconf _NPROCESSORS_ONLN)"
        make install
    )
fi

export ASAN_OPTIONS=detect_leaks=0:handle_segv=0:handle_sigbus=0
export PYTHON="$asan_dir/install/bin/python3"
export RUSTUP_TOOLCHAIN=${ASAN_RUST_TOOLCHAIN:-nightly-2026-10-25}
export CARGO_BUILD_TARGET
CARGO_BUILD_TARGET=$(rustc -vV | sed -n 's/^host: //p')
export CARGO_TARGET_DIR="$asan_dir/target"
export CARGO_UNSTABLE_BUILD_STD=std,panic_abort
export CFLAGS="-fsanitize=address -fsanitize-recover=address -fno-omit-frame-pointer -g"
export CXXFLAGS="$CFLAGS"
export RUSTFLAGS="-Zsanitizer=address -Zsanitizer-recover=address -Cforce-frame-pointers=yes -Cdebuginfo=2"

cd "$repo_dir"
cargo test --manifest-path kindasafe/Cargo.toml --locked --release --target "$CARGO_BUILD_TARGET" --workspace
python_include_dir=$("$PYTHON" -c 'import sysconfig; print(sysconfig.get_path("include"))')
"$CXX" -std=c++20 -O2 -fsanitize=address -fno-omit-frame-pointer -g -DPL_LINUX \
    -I "$python_include_dir" \
    -I dd-trace-py/ddtrace/internal/datadog/profiling/stack/echion \
    integration-test/testdata/snapshot_asan.cpp \
    dd-trace-py/ddtrace/internal/datadog/profiling/stack/src/echion/danger.cc \
    -o "$asan_dir/snapshot-asan"
ASAN_OPTIONS="$ASAN_OPTIONS:detect_stack_use_after_return=1" "$asan_dir/snapshot-asan"
if "$asan_dir/snapshot-asan" bad-destination > "$asan_dir/snapshot-destination.log" 2>&1; then
    echo "error: ASAN did not catch an invalid destination write" >&2
    exit 1
fi
grep -q 'ERROR: AddressSanitizer: heap-buffer-overflow' "$asan_dir/snapshot-destination.log"
grep -q 'WRITE of size' "$asan_dir/snapshot-destination.log"

"$PYTHON" -m pip wheel --no-deps --wheel-dir "$asan_dir/wheels" .
"$PYTHON" -m pip install --force-reinstall --no-index --only-binary=:all: --find-links "$asan_dir/wheels" pyroscope-io
python3 integration-test/download_pyroscope.py "$asan_dir/backend"
export PYROSCOPE_BINARY="$asan_dir/backend/pyroscope"
cd integration-test
go test -v -timeout 60m -count=1 -run "${RUN:-.}" ./... -profiler="${PROFILER:-all}"
