#!/bin/bash
set -euxo pipefail

for PYBIN in /opt/python/cp{310,311,312,313,314}*/bin; do
    rm -rf build/
    "${PYBIN}/pip" install --user build
    "${PYBIN}/python" -m build --wheel
done

auditwheel repair dist/*.whl --wheel-dir dist-repaired/
