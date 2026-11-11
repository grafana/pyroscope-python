FROM python:3.13.7-slim-trixie
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake git curl ca-certificates pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --profile minimal --default-toolchain 1.99.1
ENV PATH="/root/.cargo/bin:${PATH}"
WORKDIR /src/profiler
COPY profiler/ ./
RUN python -m pip install --no-cache-dir . \
    Django==5.2.8 djangorestframework==3.16.1 gunicorn==23.0.0 \
    asgiref==3.13.0 packaging==26.3 psutil==7.2.2 sqlparse==0.6.1
COPY app/ /app/
COPY harness/ /harness/
WORKDIR /app
ENV PYTHONUNBUFFERED=1
ENTRYPOINT ["python", "/harness/measure.py"]
