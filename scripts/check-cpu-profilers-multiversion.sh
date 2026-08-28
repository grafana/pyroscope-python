#!/bin/bash
# Compile the vendored CPU profiler against each CPython in the CI matrix.
#
# It reads version-specific CPython internals (_PyInterpreterFrame, f_code vs
# f_executable, cframe vs current_frame, the PEP 657 line table, ...), so a
# change that builds against the local interpreter can still break on another
# version. This catches that in a couple of minutes instead of waiting for a
# full wheel matrix. Run it after every re-vendor and after touching anything
# under cpp/cpu/.
#
# Usage: scripts/check-cpu-profilers-multiversion.sh [version...]
#
# Requires Docker. Uses the full python:X images because the -slim ones do not
# ship the CPython internal headers.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
VERSIONS=("$@")
if [ ${#VERSIONS[@]} -eq 0 ]; then
    VERSIONS=(3.10 3.11 3.12 3.13 3.14)
fi

# The project is Linux-only (see the TODO(macos) in
# cpp/cpu/ddtrace_stack/src/echion/threads.cc); this script runs in Linux
# containers, so it is expected to build for every version.
PROJECTS=(ddtrace_stack)

overall=0
for version in "${VERSIONS[@]}"; do
    echo "################ CPython ${version} ################"
    docker run --rm -v "${REPO}":/src:ro -w /tmp "python:${version}" bash -c '
        set -e
        pip install --quiet --disable-pip-version-check cmake >/dev/null 2>&1
        export PATH=$PATH:$(python -c "import sysconfig; print(sysconfig.get_path(\"scripts\"))")
        rc=0
        for proj in '"${PROJECTS[*]}"'; do
            rm -rf "/tmp/b-$proj"
            if cmake -S "/src/cpp/cpu/$proj" -B "/tmp/b-$proj" \
                    -DPython3_EXECUTABLE="$(which python)" \
                    -DPython3_FIND_STRATEGY=LOCATION >"/tmp/cfg-$proj.log" 2>&1 \
                && cmake --build "/tmp/b-$proj" -j"$(nproc)" >"/tmp/bld-$proj.log" 2>&1; then
                echo "  OK    $proj"
            else
                echo "  FAIL  $proj"
                grep -E "error:" "/tmp/cfg-$proj.log" "/tmp/bld-$proj.log" 2>/dev/null | head -10
                rc=1
            fi
        done
        exit $rc
    '
    [ $? -ne 0 ] && overall=1
done

if [ $overall -eq 0 ]; then
    echo "the CPU profiler builds on all requested CPython versions"
else
    echo "FAILURES above" >&2
fi
exit $overall
