# syntax=docker/dockerfile:1.28@sha256:bb22d9815c728170f72750f4e5b0d672e06176142e1d602c7e66c050100b7e5b
ARG PLATFORM=x86_64
FROM quay.io/pypa/musllinux_1_2_${PLATFORM}:2026.10.26-1 AS builder
ARG OPENSSL_VERSION=3.5.9

RUN apk add --no-cache gcc musl-dev make perl linux-headers
RUN manylinux-install-clang -v v22.1.8.1

ENV CC=clang
ENV CXX=clang++

# Build OpenSSL from source
RUN curl -fsSL "https://github.com/openssl/openssl/releases/download/openssl-${OPENSSL_VERSION}/openssl-${OPENSSL_VERSION}.tar.gz" \
    -o /tmp/openssl.tar.gz \
    && tar xzf /tmp/openssl.tar.gz -C /tmp \
    && cd /tmp/openssl-${OPENSSL_VERSION} \
    && ./config no-shared no-tests --prefix=/usr/local/openssl --openssldir=/etc/ssl \
    && make -j$(nproc) \
    && make install_sw \
    && ln -sf /usr/local/openssl/lib64 /usr/local/openssl/lib || true \
    && cd / \
    && rm -rf /tmp/openssl*

ENV OPENSSL_DIR=/usr/local/openssl
ENV OPENSSL_STATIC=1

RUN adduser -D builder \
    && mkdir -p /pyroscope-python \
    && chown builder:builder /pyroscope-python

USER builder
RUN test "$(id -u)" = "1000" || { echo "ERROR: builder uid is $(id -u), expected 1000"; exit 1; }

ENV RUST_VERSION=1.99.1
RUN curl https://static.rust-lang.org/rustup/dist/$(arch)-unknown-linux-musl/rustup-init -o /tmp/rustup-init \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --default-toolchain=${RUST_VERSION} --default-host=$(arch)-unknown-linux-musl \
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
