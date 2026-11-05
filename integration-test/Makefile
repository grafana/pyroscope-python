PROFILER ?= all
PYTHON ?= python3
PYROSCOPE_BINARY ?= $(CURDIR)/../.cache/pyroscope/pyroscope

.PHONY: setup
setup:
	$(PYTHON) -m pip wheel --no-deps --wheel-dir ../dist ..
	$(PYTHON) -m pip install --force-reinstall --no-index --only-binary=:all: --find-links ../dist pyroscope-io
	$(PYTHON) download_pyroscope.py $(dir $(PYROSCOPE_BINARY))

.PHONY: test test/%
test:
	PYTHON=$(PYTHON) PYROSCOPE_BINARY=$(PYROSCOPE_BINARY) go test -v -timeout 45m -count=1 ./... -profiler='$(PROFILER)'

test/%:
	PYTHON=$(PYTHON) PYROSCOPE_BINARY=$(PYROSCOPE_BINARY) go test -v -timeout 20m -count=1 -run '^$*$$' . -profiler='$(PROFILER)'
