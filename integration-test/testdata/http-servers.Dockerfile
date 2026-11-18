ARG PYTHON_IMAGE=python:3.15-slim
FROM ${PYTHON_IMAGE}

# These server fixtures use only universal Python wheels, including on musl.
COPY http-servers-requirements.txt /tmp/http-servers-requirements.txt
RUN python -m pip install --no-cache-dir --only-binary=:all: --require-hashes \
    -r /tmp/http-servers-requirements.txt
