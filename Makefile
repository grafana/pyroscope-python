.PHONY: ffi/python/header
ffi/python/header:
	cd rust && cbindgen --config cbindgen.toml --output include/pyroscope_ffi.h

.PHONY: ffi/python/cffi
ffi/python/cffi:
	python scripts/tests/compile_ffi.py

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

.PHONY: mac/amd64
mac/amd64:
	MACOSX_DEPLOYMENT_TARGET=11.0 CARGO_BUILD_TARGET=x86_64-apple-darwin python -m build --wheel
	wheel tags --platform-tag macosx_11_0_x86_64 --remove dist/*.whl

.PHONY: mac/arm64
mac/arm64:
	MACOSX_DEPLOYMENT_TARGET=11.0 CARGO_BUILD_TARGET=aarch64-apple-darwin python -m build --wheel
	wheel tags --platform-tag macosx_11_0_arm64 --remove dist/*.whl

.PHONY: check/tag-version
check/tag-version:
	@TAG_VERSION=$${TAG#python-}; \
	CARGO_VERSION=$$(sed -n 's/^version = "\(.*\)"/\1/p' rust/Cargo.toml); \
	if [ "$$TAG_VERSION" != "$$CARGO_VERSION" ]; then \
		echo "error: tag version ($$TAG_VERSION) does not match Cargo.toml version ($$CARGO_VERSION)"; \
		exit 1; \
	fi; \
	echo "tag version ($$TAG_VERSION) matches Cargo.toml version ($$CARGO_VERSION)"
