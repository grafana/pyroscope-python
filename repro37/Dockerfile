# Reproducer for https://github.com/grafana/pyroscope-python/issues/37
#
# Python 3.11 + pyroscope-io 1.2.1, i.e. the combination from the reports.
# gdb is installed so a core dump produced inside the container can be
# inspected without leaving it.
FROM python:3.11-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends gdb procps \
 && rm -rf /var/lib/apt/lists/*

ARG PYROSCOPE_VERSION=1.2.1
RUN pip install --no-cache-dir "pyroscope-io==${PYROSCOPE_VERSION}" gunicorn

WORKDIR /app
COPY sink.py tls_sink.py workload.py wsgi.py gunicorn_conf.py runner.py forkload.py \
     poc_frame_cycle.py poc_ucs4_garbage.py poc_hostile_string.py poc_shutdown_race.py \
     poc_niche_confusion.py ./

ENV PYTHONUNBUFFERED=1 \
    PYROSCOPE_SERVER=http://127.0.0.1:4040 \
    SAMPLE_RATE=997

# The runner drives the workers; cores land in /app when the container is run
# with --ulimit core=-1.
ENTRYPOINT ["python", "runner.py"]
CMD ["--mode", "threads", "--procs", "8", "--duration", "60", "--budget", "3600"]
