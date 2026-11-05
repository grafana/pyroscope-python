.PHONY: ffi/python/header
ffi/python/header:
	cd rust && cbindgen --config cbindgen.toml --output include/pyroscope_ffi.h

.PHONY: linux/amd64
linux/amd64:
	docker buildx build \
		--build-arg=PLATFORM=x86_64 \
		--platform=linux/amd64 \
		--output=. \
		-f docker/wheel.Dockerfile \
		.

.PHONY: linux/arm64
linux/arm64:
	docker buildx build \
		--build-arg=PLATFORM=aarch64 \
		--platform=linux/arm64 \
		--output=. \
		-f docker/wheel.Dockerfile \
		.

.PHONY: musllinux/amd64
musllinux/amd64:
	docker buildx build \
		--build-arg=PLATFORM=x86_64 \
		--platform=linux/amd64 \
		--output=. \
		-f docker/wheel-musllinux.Dockerfile \
		.

.PHONY: musllinux/arm64
musllinux/arm64:
	docker buildx build \
		--build-arg=PLATFORM=aarch64 \
		--platform=linux/arm64 \
		--output=. \
		-f docker/wheel-musllinux.Dockerfile \
		.

ASAN_PYTHON_REF ?= 3.14
PROFILER ?= all

.PHONY: asan/test
asan/test:
	ASAN_PYTHON_REF=$(ASAN_PYTHON_REF) PROFILER='$(PROFILER)' RUN='$(RUN)' bash scripts/asan-test.sh

PYTHON ?= python3

.PHONY: mac/amd64
mac/amd64:
	rm -rf build dist/mac
	MACOSX_DEPLOYMENT_TARGET=11.0 CARGO_BUILD_TARGET=x86_64-apple-darwin $(PYTHON) -m build --wheel --outdir dist/mac
	$(PYTHON) -m wheel tags --platform-tag macosx_11_0_x86_64 --remove dist/mac/*.whl
	mv dist/mac/*.whl dist/
	rm -rf dist/mac

.PHONY: mac/arm64
mac/arm64:
	rm -rf build dist/mac
	MACOSX_DEPLOYMENT_TARGET=11.0 CARGO_BUILD_TARGET=aarch64-apple-darwin $(PYTHON) -m build --wheel --outdir dist/mac
	$(PYTHON) -m wheel tags --platform-tag macosx_11_0_arm64 --remove dist/mac/*.whl
	mv dist/mac/*.whl dist/
	rm -rf dist/mac

.PHONY: check/tag-version
check/tag-version:
	@TAG_VERSION=$${TAG#python-}; \
	CARGO_VERSION=$$(cd rust && cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.name == "pyroscope_python_extension") | .version'); \
	if [ "$$TAG_VERSION" != "$$CARGO_VERSION" ]; then \
		echo "error: tag version ($$TAG_VERSION) does not match Cargo.toml version ($$CARGO_VERSION)"; \
		exit 1; \
	fi; \
	echo "tag version ($$TAG_VERSION) matches Cargo.toml version ($$CARGO_VERSION)"
