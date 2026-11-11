FROM pyroscope-python-bench:f6a0e3e
WORKDIR /src/profiler
COPY profiler/ ./
RUN python -m pip wheel --no-deps --wheel-dir /fixed-wheel .
