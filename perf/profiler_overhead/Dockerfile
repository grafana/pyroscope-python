FROM rust:1.96-slim-bookworm@sha256:39decc9f9f4a87e03db5069b2424c0ee13c3d7e4116010980d4f96849e823bc4 AS rust

FROM python:3.13-slim-bookworm@sha256:b6bd71b0dd3811ddbcbc523ec2965fd1e1bcfdf7a20ab24679273d3bee726129 AS builder

COPY --from=rust /usr/local/cargo /usr/local/cargo
COPY --from=rust /usr/local/rustup /usr/local/rustup
ENV PATH=/usr/local/cargo/bin:${PATH} \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PIP_DISABLE_PIP_VERSION_CHECK=1

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential cmake git libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY pyproject.toml setup.py README.md LICENSE MANIFEST.in ./
COPY python/ python/
COPY rust/ rust/
COPY cpp/ cpp/

RUN python -m pip install --no-cache-dir \
        setuptools==83.0.0 \
        setuptools-rust==1.12.0 \
        wheel==0.45.1 \
    && mkdir /wheelhouse \
    && python -m pip wheel --no-cache-dir --wheel-dir /wheelhouse \
        gunicorn==26.2.0 \
        packaging==26.3 \
    && python -m pip wheel --no-cache-dir --no-build-isolation --no-deps --wheel-dir /wheelhouse .

FROM python:3.13-slim-bookworm@sha256:b6bd71b0dd3811ddbcbc523ec2965fd1e1bcfdf7a20ab24679273d3bee726129

ENV PIP_DISABLE_PIP_VERSION_CHECK=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1

COPY --from=builder /wheelhouse /wheelhouse
RUN python -m pip install --no-cache-dir --no-index --find-links=/wheelhouse pyroscope-io gunicorn \
    && rm -rf /wheelhouse

COPY perf/profiler_overhead/bench /opt/profiler-overhead/bench
WORKDIR /opt/profiler-overhead

CMD ["python", "-m", "bench.target", "--help"]
