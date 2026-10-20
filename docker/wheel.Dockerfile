# syntax=docker/dockerfile:1.28@sha256:bb22d9815c728170f72750f4e5b0d672e06176142e1d602c7e66c050100b7e5b
ARG PLATFORM=x86_64
FROM quay.io/pypa/manylinux2014_${PLATFORM}:2026.09.26-1 AS manylinux
RUN manylinux-install-clang -v v22.1.8.1

FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f AS openssl
ARG OPENSSL_VERSION=4.0.3

RUN apt-get update \
    && apt-get install -y --no-install-recommends perl make curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ENV PATH=/opt/clang/bin:$PATH
ENV CC=clang
ENV SOURCE_DATE_EPOCH=0

# Build OpenSSL from source
RUN --mount=type=bind,from=manylinux,source=/opt/clang,target=/opt/clang \
    --mount=type=bind,from=manylinux,source=/usr/include,target=/usr/include \
    --mount=type=bind,from=manylinux,source=/opt/rh/devtoolset-10,target=/opt/rh/devtoolset-10 \
    curl -fsSL "https://github.com/openssl/openssl/releases/download/openssl-${OPENSSL_VERSION}/openssl-${OPENSSL_VERSION}.tar.gz" \
    -o /tmp/openssl.tar.gz \
    && tar xzf /tmp/openssl.tar.gz -C /tmp \
    && cd /tmp/openssl-${OPENSSL_VERSION} \
    && ./config no-shared no-tests --prefix=/usr/local/openssl --openssldir=/etc/ssl \
    && make -j$(nproc) build_libs \
    && make install_dev \
    && ln -sf /usr/local/openssl/lib64 /usr/local/openssl/lib || true \
    && cd / \
    && rm -rf /tmp/openssl*

FROM manylinux AS builder

ENV CC=clang
ENV CXX=clang++

COPY --from=openssl /usr/local/openssl /usr/local/openssl
ENV OPENSSL_DIR=/usr/local/openssl
ENV OPENSSL_STATIC=1

RUN useradd -m builder \
    && mkdir -p /pyroscope-python \
    && chown builder:builder /pyroscope-python

USER builder
RUN test "$(id -u)" = "1000" || { echo "ERROR: builder uid is $(id -u), expected 1000"; exit 1; }

ENV RUST_VERSION=1.98.0
RUN curl https://static.rust-lang.org/rustup/dist/$(arch)-unknown-linux-musl/rustup-init -o /tmp/rustup-init \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --default-toolchain=${RUST_VERSION} --default-host=$(arch)-unknown-linux-gnu \
    && rm /tmp/rustup-init
ENV PATH=/home/builder/.cargo/bin:$PATH

WORKDIR /pyroscope-python

ADD --chown=builder:builder pyproject.toml \
    setup.py \
    ./

ADD --chown=builder:builder rust/ rust/
ADD --chown=builder:builder python/ python/
ADD --chown=builder:builder dd-trace-py/ dd-trace-py/
ADD --chown=builder:builder docker/wheels.sh wheels.sh

RUN --mount=type=cache,target=/home/builder/.cargo/registry,uid=1000,gid=1000 \
    --mount=type=cache,target=/home/builder/.cargo/git,uid=1000,gid=1000 \
    bash wheels.sh

FROM scratch
COPY --from=builder  /pyroscope-python/dist-repaired dist/
