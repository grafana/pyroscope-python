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

TSAN_PYTHON_REF ?= 3.14
TSAN_IMAGE ?= pyroscope-python-tsan:$(TSAN_PYTHON_REF)
TSAN_WHEEL_DIR := dist/tsan
TSAN_FAST_COPY_WARMUP ?= 0

.PHONY: tsan/image
tsan/image:
	docker buildx build \
		--build-arg=PYTHON_REF=$(TSAN_PYTHON_REF) \
		--target=runtime \
		--load \
		-t $(TSAN_IMAGE) \
		-f docker/tsan.Dockerfile \
		.

.PHONY: tsan/wheel
tsan/wheel:
	rm -rf $(TSAN_WHEEL_DIR)
	docker buildx build \
		--build-arg=PYTHON_REF=$(TSAN_PYTHON_REF) \
		--target=wheel \
		--output=$(TSAN_WHEEL_DIR) \
		-f docker/tsan.Dockerfile \
		.

.PHONY: tsan/test
tsan/test: tsan/image tsan/wheel
	cd integration-test && \
		PYTHON_IMAGE=$(TSAN_IMAGE) \
		PYROSCOPE_WHEEL_DIR=../$(TSAN_WHEEL_DIR) \
		PYROSCOPE_WHEEL_GLOB='*.whl' \
		PYROSCOPE_FAST_COPY_WARMUP=$(TSAN_FAST_COPY_WARMUP) \
		go test -v -timeout 60m -count=1 $(if $(RUN),-run '$(RUN)') .

.PHONY: mac/amd64
mac/amd64:
	MACOSX_DEPLOYMENT_TARGET=11.0 CARGO_BUILD_TARGET=x86_64-apple-darwin python3 -m build --wheel
	wheel tags --platform-tag macosx_11_0_x86_64 --remove dist/*.whl

.PHONY: mac/arm64
mac/arm64:
	MACOSX_DEPLOYMENT_TARGET=11.0 CARGO_BUILD_TARGET=aarch64-apple-darwin python3 -m build --wheel
	wheel tags --platform-tag macosx_11_0_arm64 --remove dist/*.whl

.PHONY: check/tag-version
check/tag-version:
	@TAG_VERSION=$${TAG#python-}; \
	CARGO_VERSION=$$(cd rust && cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'); \
	if [ "$$TAG_VERSION" != "$$CARGO_VERSION" ]; then \
		echo "error: tag version ($$TAG_VERSION) does not match Cargo.toml version ($$CARGO_VERSION)"; \
		exit 1; \
	fi; \
	echo "tag version ($$TAG_VERSION) matches Cargo.toml version ($$CARGO_VERSION)"
