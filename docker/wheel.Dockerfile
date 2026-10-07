# syntax=docker/dockerfile:1.23@sha256:2780b5c3bab67f1f76c781860de469442999ed1a0d7992a5efdf2cffc0e3d769
ARG PLATFORM=x86_64
ARG OPENSSL_VERSION=3.5.9

FROM quay.io/pypa/manylinux2014_${PLATFORM} AS sysroot

FROM --platform=$BUILDPLATFORM debian:bookworm-slim AS openssl
ARG PLATFORM
ARG OPENSSL_VERSION
ARG CLANG_VERSION=v22.1.8.1
RUN apt-get update \
    && apt-get install -y --no-install-recommends perl make curl ca-certificates xz-utils \
    && rm -rf /var/lib/apt/lists/*
COPY --from=sysroot /opt/_internal/build_scripts/static_clang_versions.txt /tmp/
RUN case "$(uname -m)" in x86_64) arch=amd64;; aarch64) arch=arm64;; esac \
    && url="https://github.com/mayeut/static-clang-images/releases/download/${CLANG_VERSION}" \
    && cd /tmp \
    && curl -fsSLO --retry 10 "$url/sha256sums.txt" \
    && echo "$(awk -v v=${CLANG_VERSION} '$1 == v { print $2 }' static_clang_versions.txt)  sha256sums.txt" | sha256sum -c - \
    && curl -fsSLO --retry 10 "$url/static-clang-linux-$arch.tar.xz" \
    && grep " static-clang-linux-$arch.tar.xz\$" sha256sums.txt | sha256sum -c - \
    && tar -C /opt -xJf "static-clang-linux-$arch.tar.xz" \
    && rm -f /tmp/*
ENV PATH=/opt/clang/bin:$PATH
COPY --from=sysroot /usr/include /sysroot/usr/include
RUN curl -fsSL "https://github.com/openssl/openssl/releases/download/openssl-${OPENSSL_VERSION}/openssl-${OPENSSL_VERSION}.tar.gz" \
    -o /tmp/openssl.tar.gz \
    && tar xzf /tmp/openssl.tar.gz -C /tmp \
    && cd /tmp/openssl-${OPENSSL_VERSION} \
    && CC="clang --target=${PLATFORM}-linux-gnu --sysroot=/sysroot" \
       ./Configure linux-${PLATFORM} no-shared no-tests \
       --prefix=/usr/local/openssl --libdir=lib --openssldir=/etc/ssl \
    && make -j$(nproc) build_libs \
    && make install_dev

FROM quay.io/pypa/manylinux2014_${PLATFORM} AS builder

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
