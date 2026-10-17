# syntax=docker/dockerfile:1
FROM ubuntu:26.04 AS toolchain
ARG LLVM_VERSION=22

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        clang-${LLVM_VERSION} llvm-${LLVM_VERSION} libclang-rt-${LLVM_VERSION}-dev g++ \
        make cmake git curl ca-certificates pkg-config \
        libssl-dev libffi-dev zlib1g-dev libbz2-dev liblzma-dev libzstd-dev \
        libsqlite3-dev libreadline-dev libncurses-dev libgdbm-dev uuid-dev \
    && rm -rf /var/lib/apt/lists/*

ENV PATH=/usr/lib/llvm-${LLVM_VERSION}/bin:$PATH
ENV CC=clang
ENV CXX=clang++

FROM toolchain AS runtime
ARG PYTHON_REF=3.14

RUN git clone --depth 1 --branch ${PYTHON_REF} https://github.com/python/cpython.git /tmp/cpython \
    && cd /tmp/cpython \
    && ./configure --prefix=/usr/local --enable-shared --with-thread-sanitizer --with-ensurepip=install \
    && make -j"$(nproc)" \
    && make install \
    && ldconfig \
    && ln -s python3 /usr/local/bin/python \
    && mkdir -p /usr/local/share/tsan \
    && cp Tools/tsan/suppressions.txt /usr/local/share/tsan/suppressions.txt \
    && cd / \
    && rm -rf /tmp/cpython

ENV TSAN_OPTIONS=halt_on_error=0:exitcode=0:die_after_fork=0:handle_segv=0:handle_sigbus=0:suppressions=/usr/local/share/tsan/suppressions.txt

FROM runtime AS wheel-build
ARG RUST_TOOLCHAIN=nightly

ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_TOOLCHAIN} --component rust-src \
    && printf '[unstable]\nbuild-std = ["std", "panic_abort"]\n' > ${CARGO_HOME}/config.toml

ENV CFLAGS="-fsanitize=thread -fno-omit-frame-pointer -g"
ENV CXXFLAGS="-fsanitize=thread -fno-omit-frame-pointer -g"
ENV RUSTFLAGS="-Zsanitizer=thread -Cforce-frame-pointers=yes -Cdebuginfo=2"

WORKDIR /pyroscope-python
COPY pyproject.toml setup.py ./
COPY rust/ rust/
COPY python/ python/
COPY dd-trace-py/ dd-trace-py/

RUN --mount=type=cache,target=/opt/cargo/registry \
    --mount=type=cache,target=/opt/cargo/git \
    CARGO_BUILD_TARGET="$(uname -m)-unknown-linux-gnu" \
    python -m pip wheel --no-deps --wheel-dir /dist .

FROM scratch AS wheel
COPY --from=wheel-build /dist/ /
