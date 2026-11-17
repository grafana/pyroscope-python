ARG PYTHON_IMAGE=python:3.15-slim
FROM ${PYTHON_IMAGE}

# Compile against the selected interpreter (including the ASAN runtime), not
# a distribution's Python. These dependencies belong only to the uWSGI test.
RUN if [ -f /etc/alpine-release ]; then \
        apk add --no-cache build-base linux-headers; \
    else \
        apt-get update \
        && apt-get install -y --no-install-recommends gcc libc6-dev libcrypt-dev \
        && rm -rf /var/lib/apt/lists/*; \
    fi

RUN python -m pip install --no-cache-dir setuptools==84.0.0 wheel==0.48.0 packaging==26.0

RUN python -m pip download --no-cache-dir --no-deps --no-build-isolation --dest /tmp \
        'https://files.pythonhosted.org/packages/9f/49/2f57640e889ba509fd1fae10cccec1b58972a07c2724486efba94c5ea448/uwsgi-2.0.31.tar.gz#sha256=e8f8b350ccc106ff93a65247b9136f529c14bf96b936ac5b264c6ff9d0c76257'

# Build both layouts from the same verified source and interpreter. Clean the
# objects between builds so the core cannot retain an embedded Python plugin.
# uWSGI does not inherit Python's linker flags; ASAN needs its runtime linked
# into the executable even when libpython is loaded later through the plugin.
RUN tar -xzf /tmp/uwsgi-2.0.31.tar.gz -C /tmp \
    && cd /tmp/uwsgi-2.0.31 \
    && export CPUCOUNT=2 LDFLAGS="$(python -c 'import sysconfig; print(sysconfig.get_config_var("LDFLAGS") or "")')" \
    && UWSGI_BIN_NAME=/usr/local/bin/uwsgi python uwsgiconfig.py --build default \
    && python uwsgiconfig.py --clean \
    && UWSGI_BIN_NAME=/usr/local/bin/uwsgi-core python uwsgiconfig.py --build core \
    && python uwsgiconfig.py --plugin plugins/python core \
    && mkdir -p /usr/local/lib/uwsgi \
    && mv python_plugin.so /usr/local/lib/uwsgi/ \
    && cd / \
    && rm -rf /tmp/uwsgi-2.0.31 /tmp/uwsgi-2.0.31.tar.gz

# Verify the loading distinction: the core has no libpython dependency; the
# embedded executable and shared plugin both do.
RUN readelf -d /usr/local/bin/uwsgi | grep -F 'Shared library: [libpython' \
    && readelf -d /usr/local/bin/uwsgi-core > /tmp/uwsgi-core.dynamic \
    && ! grep -F 'Shared library: [libpython' /tmp/uwsgi-core.dynamic \
    && readelf -d /usr/local/lib/uwsgi/python_plugin.so | grep -F 'Shared library: [libpython' \
    && rm /tmp/uwsgi-core.dynamic
