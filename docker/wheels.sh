#!/bin/bash
set -euxo pipefail

for tag in cp310-cp310 cp311-cp311 cp312-cp312 cp313-cp313 cp314-cp314; do
    PYBIN="/opt/python/${tag}/bin"
    rm -rf build/
    "${PYBIN}/pip" install --user build
    "${PYBIN}/python" -m build --wheel
done

auditwheel repair dist/*.whl --wheel-dir dist-repaired/
