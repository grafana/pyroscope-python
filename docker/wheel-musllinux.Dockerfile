# syntax=docker/dockerfile:1.23@sha256:2780b5c3bab67f1f76c781860de469442999ed1a0d7992a5efdf2cffc0e3d769
ARG PLATFORM=x86_64
FROM quay.io/pypa/musllinux_1_2_${PLATFORM} AS builder
ARG OPENSSL_VERSION=3.5.5

RUN apk add --no-cache gcc musl-dev libffi-dev make perl linux-headers

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

ENV RUST_VERSION=1.88
RUN curl https://static.rust-lang.org/rustup/dist/$(arch)-unknown-linux-musl/rustup-init -o /tmp/rustup-init \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --default-toolchain=${RUST_VERSION} --default-host=$(arch)-unknown-linux-musl \
    && rm /tmp/rustup-init
ENV PATH=/home/builder/.cargo/bin:$PATH

WORKDIR /pyroscope-python

RUN /opt/python/cp310-cp310/bin/python -m pip install --user build

ADD --chown=builder:builder pyproject.toml \
    setup.py \
    ./

ADD --chown=builder:builder rust/ rust/
ADD --chown=builder:builder python/ python/

RUN --mount=type=cache,target=/home/builder/.cargo/registry,uid=1000,gid=1000 \
    --mount=type=cache,target=/home/builder/.cargo/git,uid=1000,gid=1000 \
    /opt/python/cp310-cp310/bin/python -m build --wheel

RUN auditwheel repair dist/*.whl --wheel-dir dist-repaired/

FROM scratch
COPY --from=builder  /pyroscope-python/dist-repaired dist/
