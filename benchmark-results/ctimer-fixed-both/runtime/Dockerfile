FROM python:3.13.7-slim-trixie@sha256:5f55cdf0c5d9dc1a415637a5ccc4a9e18663ad203673173b8cda8f8dcacef689
RUN apt-get update && apt-get install -y --no-install-recommends libstdc++6 libssl3t64 openssl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY wheels/ /wheels/
RUN python -m pip install --no-cache-dir /wheels/*.whl Django==5.2.8 djangorestframework==3.16.1 gunicorn==23.0.0 asgiref==3.13.0 packaging==26.3 psutil==7.2.2 sqlparse==0.6.1
COPY app/ /app/
WORKDIR /app
ENV PYTHONUNBUFFERED=1
ENTRYPOINT ["python", "/harness/measure.py"]
