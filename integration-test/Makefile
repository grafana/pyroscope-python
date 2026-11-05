PROFILER ?= all
PYTHON ?= python3
PYROSCOPE_BINARY ?= $(CURDIR)/../.cache/pyroscope/pyroscope
PYROSCOPE_VERSION := 2.3.1
PYROSCOPE_OS := $(shell uname -s | tr '[:upper:]' '[:lower:]')
PYROSCOPE_ARCH := $(subst aarch64,arm64,$(subst x86_64,amd64,$(shell uname -m)))

.PHONY: setup
setup: download-pyroscope
	$(PYTHON) -m pip wheel --no-deps --wheel-dir ../dist ..
	$(PYTHON) -m pip install --force-reinstall --no-index --only-binary=:all: --find-links ../dist pyroscope-io

.PHONY: download-pyroscope
download-pyroscope:
	mkdir -p "$(dir $(PYROSCOPE_BINARY))"
	wget -q -O "$(PYROSCOPE_BINARY).tar.gz" "https://github.com/grafana/pyroscope/releases/download/v$(PYROSCOPE_VERSION)/pyroscope_$(PYROSCOPE_VERSION)_$(PYROSCOPE_OS)_$(PYROSCOPE_ARCH).tar.gz"
	tar -xOzf "$(PYROSCOPE_BINARY).tar.gz" pyroscope > "$(PYROSCOPE_BINARY)"
	chmod +x "$(PYROSCOPE_BINARY)"
	rm "$(PYROSCOPE_BINARY).tar.gz"

.PHONY: test test/%
test:
	PYTHON=$(PYTHON) PYROSCOPE_BINARY=$(PYROSCOPE_BINARY) go test -v -timeout 45m -count=1 ./... -profiler='$(PROFILER)'

test/%:
	PYTHON=$(PYTHON) PYROSCOPE_BINARY=$(PYROSCOPE_BINARY) go test -v -timeout 20m -count=1 -run '^$*$$' . -profiler='$(PROFILER)'
