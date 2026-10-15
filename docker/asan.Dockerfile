# syntax=docker/dockerfile:1
FROM scratch AS pyspy

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
ENV ASAN_SYMBOLIZER_PATH=/usr/lib/llvm-${LLVM_VERSION}/bin/llvm-symbolizer
ENV ASAN_OPTIONS=detect_leaks=0

FROM toolchain AS runtime
ARG PYTHON_REF=3.14

RUN git clone --depth 1 --branch ${PYTHON_REF} https://github.com/python/cpython.git /tmp/cpython \
    && cd /tmp/cpython \
    && ./configure --prefix=/usr/local --enable-shared --with-address-sanitizer --without-pymalloc --with-ensurepip=install \
    && make -j"$(nproc)" \
    && make install \
    && ldconfig \
    && ln -s python3 /usr/local/bin/python \
    && cd / \
    && rm -rf /tmp/cpython

ENV ASAN_OPTIONS=detect_leaks=0:handle_segv=0:handle_sigbus=0

FROM runtime AS wheel-build
ARG RUST_TOOLCHAIN=nightly

ENV RUSTUP_HOME=/opt/rustup
ENV CARGO_HOME=/opt/cargo
ENV PATH=/opt/cargo/bin:$PATH
RUN curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain ${RUST_TOOLCHAIN} --component rust-src \
    && printf '[unstable]\nbuild-std = ["std", "panic_abort"]\n' > ${CARGO_HOME}/config.toml

ENV CFLAGS="-fsanitize=address -fno-omit-frame-pointer -g"
ENV CXXFLAGS="-fsanitize=address -fno-omit-frame-pointer -g"
ENV RUSTFLAGS="-Zsanitizer=address -Cforce-frame-pointers=yes -Cdebuginfo=2"

WORKDIR /pyroscope-python
COPY pyproject.toml setup.py ./
COPY rust/ rust/
COPY python/ python/
COPY dd-trace-py/ dd-trace-py/

COPY --from=pyspy / /pyspy/

RUN --mount=type=cache,target=/opt/cargo/registry \
    --mount=type=cache,target=/opt/cargo/git \
    if [ -f /pyspy/Cargo.toml ]; then \
        printf '[patch."https://github.com/grafana/pyroscope-py-spy"]\npy-spy = { path = "/pyspy" }\n' >> ${CARGO_HOME}/config.toml \
        && cargo update -p py-spy --manifest-path rust/Cargo.toml; \
    fi \
    && CARGO_BUILD_TARGET="$(uname -m)-unknown-linux-gnu" \
    python -m pip wheel --no-deps --wheel-dir /dist .

FROM scratch AS wheel
COPY --from=wheel-build /dist/ /
