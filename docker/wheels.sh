#!/bin/bash
set -euxo pipefail

# Without this clang takes libstdc++ headers from whichever gcc it finds first,
# which is not the one the rust link step pulls libstdc++.a out of.
if [ "${CC:-}" = clang ]; then
    gcc_install_dir="$(dirname "$(gcc -print-libgcc-file-name)")"
    export CFLAGS="--gcc-install-dir=$gcc_install_dir"
    export CXXFLAGS="--gcc-install-dir=$gcc_install_dir"
fi

for tag in cp310-cp310 cp311-cp311 cp312-cp312 cp313-cp313 cp314-cp314; do
    PYBIN="/opt/python/${tag}/bin"
    rm -rf build/
    "${PYBIN}/pip" install --user build
    "${PYBIN}/python" -m build --wheel
done

auditwheel repair dist/*.whl --wheel-dir dist-repaired/
