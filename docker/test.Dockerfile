# syntax=docker/dockerfile:1.23@sha256:2780b5c3bab67f1f76c781860de469442999ed1a0d7992a5efdf2cffc0e3d769

ARG CPYTHON_VERSION=3.13.14
ARG CPYTHON_SHA256=5ae535a36af0ebca6fca176ecb8197f5db9c1cb8c8f0cd12cdf1787046db1f41
ARG RUST_VERSION=1.96.0
ARG RUST_NIGHTLY=nightly-2026-06-15
ARG RUST_TARGET=x86_64-unknown-linux-gnu
ARG BUILD_CONFIG=debug

FROM python:3.13-slim-trixie AS toolchain

ARG RUST_VERSION
ARG RUST_NIGHTLY

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        binutils \
        ca-certificates \
        clang \
        cmake \
        curl \
        git \
        libbz2-dev \
        libclang-rt-dev \
        libffi-dev \
        liblzma-dev \
        libreadline-dev \
        libsqlite3-dev \
        libssl-dev \
        pkg-config \
        tk-dev \
        uuid-dev \
        xz-utils \
        zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl --proto '=https' --tlsv1.2 -fsS https://sh.rustup.rs -o /tmp/rustup-init.sh \
    && sh /tmp/rustup-init.sh -y --profile minimal --default-toolchain "${RUST_VERSION}" \
    && rm /tmp/rustup-init.sh \
    && /root/.cargo/bin/rustup toolchain install "${RUST_NIGHTLY}" --profile minimal --component rust-src

ENV PATH="/root/.cargo/bin:${PATH}"

FROM toolchain AS cpython-source

ARG CPYTHON_VERSION
ARG CPYTHON_SHA256

RUN curl --proto '=https' --tlsv1.2 -fsS \
        "https://www.python.org/ftp/python/${CPYTHON_VERSION}/Python-${CPYTHON_VERSION}.tgz" \
        -o /tmp/cpython.tgz \
    && echo "${CPYTHON_SHA256}  /tmp/cpython.tgz" | sha256sum --check - \
    && mkdir -p /opt/cpython-source \
    && tar -xzf /tmp/cpython.tgz --strip-components=1 -C /opt/cpython-source \
    && rm /tmp/cpython.tgz

FROM cpython-source AS cpython-asan

ENV ASAN_OPTIONS="detect_leaks=0:halt_on_error=1:allocator_may_return_null=1:handle_segv=0"
ENV PYTHONMALLOC=malloc

RUN cd /opt/cpython-source \
    && CC=clang CXX=clang++ ./configure \
        --prefix=/opt/cpython \
        --enable-shared \
        --with-address-sanitizer \
        --with-ensurepip=install \
    && make -j"$(nproc)" \
    && make install

RUN ln -s /opt/cpython/bin/python3 /opt/cpython/bin/python

ENV LD_LIBRARY_PATH="/opt/cpython/lib"
ENV PATH="/opt/cpython/bin:${PATH}"

FROM cpython-source AS cpython-tsan

ENV TSAN_OPTIONS="halt_on_error=1:exitcode=66:handle_segv=0:suppressions=/opt/cpython-source/Tools/tsan/suppressions.txt"

RUN cd /opt/cpython-source \
    && CC=clang CXX=clang++ ./configure \
        --prefix=/opt/cpython \
        --enable-shared \
        --disable-ipv6 \
        --with-thread-sanitizer \
        --with-ensurepip=install \
    && make -j"$(nproc)" \
    && make install

RUN ln -s /opt/cpython/bin/python3 /opt/cpython/bin/python

ENV LD_LIBRARY_PATH="/opt/cpython/lib"
ENV PATH="/opt/cpython/bin:${PATH}"

FROM toolchain AS configured-debug
ENV SETUPTOOLS_RUST_CARGO_PROFILE=dev

FROM toolchain AS configured-release
ENV SETUPTOOLS_RUST_CARGO_PROFILE=release

FROM cpython-asan AS configured-asan
ARG RUST_NIGHTLY
ARG RUST_TARGET
ENV CC=clang
ENV CXX=clang++
ENV CARGO_BUILD_TARGET="${RUST_TARGET}"
ENV PYROSCOPE_SANITIZER=address
ENV RUSTFLAGS="-Zsanitizer=address -Zexternal-clangrt -Cforce-frame-pointers=yes"
ENV RUST_NIGHTLY="${RUST_NIGHTLY}"
ENV RUSTUP_TOOLCHAIN="${RUST_NIGHTLY}"
ENV SETUPTOOLS_RUST_CARGO_PROFILE=dev

FROM cpython-tsan AS configured-tsan
ARG RUST_NIGHTLY
ARG RUST_TARGET
ENV CC=clang
ENV CXX=clang++
ENV CARGO_BUILD_TARGET="${RUST_TARGET}"
ENV PYROSCOPE_SANITIZER=thread
ENV RUSTFLAGS="-Zsanitizer=thread -Zexternal-clangrt -Cforce-frame-pointers=yes"
ENV RUST_NIGHTLY="${RUST_NIGHTLY}"
ENV RUSTUP_TOOLCHAIN="${RUST_NIGHTLY}"
ENV SETUPTOOLS_RUST_CARGO_PROFILE=dev

FROM configured-${BUILD_CONFIG} AS test

WORKDIR /pyroscope-python
COPY . .

RUN python3 -m pip install --no-cache-dir \
        build \
        "setuptools==83.0.0" \
        "setuptools-rust==1.12.0" \
        wheel \
    && mkdir -p /wheels \
    && python3 -m build --wheel --no-isolation --outdir /wheels

CMD ["python3"]
