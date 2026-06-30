.PHONY: ci-matrix
ci-matrix:
	@go test -list . -json . | jq -sc '[.[] | select(.Action=="output") | .Output | rtrimstr("\n") | select(startswith("Test"))]'

UNAME_MACHINE := $(shell uname -m)
WHEEL_ARCH := $(if $(filter arm64 aarch64,$(UNAME_MACHINE)),arm64,amd64)
WHEEL_PLATFORM := $(if $(findstring alpine,$(PYTHON_IMAGE_SUFFIX)),musllinux,linux)
WHEEL_TARGET := $(or $(PYROSCOPE_WHEEL_TARGET),$(WHEEL_PLATFORM)/$(WHEEL_ARCH))

.PHONY: wheel
wheel:
	@if [ -n "$(PYROSCOPE_WHEEL_DIR)" ]; then \
		echo "using prebuilt wheel from PYROSCOPE_WHEEL_DIR=$(PYROSCOPE_WHEEL_DIR)"; \
	else \
		$(MAKE) -C .. $(WHEEL_TARGET); \
	fi

.PHONY: test/%
test/%: wheel
	go test -v -timeout 10m -count=1 -run '^$*$$' .
