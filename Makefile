.PHONY: build/debug build/release build/asan build/tsan
.PHONY: test/unit/debug test/unit/release test/unit/asan test/unit/tsan

PYTHON ?= python3
RUST_NIGHTLY ?= nightly
RUST_TARGET ?= $(shell rustc -vV | awk '/^host:/ { print $$2 }')

.PHONY: build/debug
build/debug:
	SETUPTOOLS_RUST_CARGO_PROFILE=dev \
		$(PYTHON) -m pip install -e . --no-build-isolation

.PHONY: build/release
build/release:
	SETUPTOOLS_RUST_CARGO_PROFILE=release \
		$(PYTHON) -m pip install -e . --no-build-isolation

.PHONY: build/asan
build/asan:
	CC=clang CXX=clang++ \
		RUSTUP_TOOLCHAIN=$(RUST_NIGHTLY) \
		CARGO_BUILD_TARGET=$(RUST_TARGET) \
		RUSTFLAGS="-Zsanitizer=address -Zexternal-clangrt -Cforce-frame-pointers=yes" \
		PYROSCOPE_SANITIZER=address \
		SETUPTOOLS_RUST_CARGO_PROFILE=dev \
		$(PYTHON) -m pip install -e . --no-build-isolation

.PHONY: build/tsan
build/tsan:
	CC=clang CXX=clang++ \
		RUSTUP_TOOLCHAIN=$(RUST_NIGHTLY) \
		CARGO_BUILD_TARGET=$(RUST_TARGET) \
		RUSTFLAGS="-Zsanitizer=thread -Zexternal-clangrt -Cforce-frame-pointers=yes" \
		PYROSCOPE_SANITIZER=thread \
		SETUPTOOLS_RUST_CARGO_PROFILE=dev \
		$(PYTHON) -m pip install -e . --no-build-isolation

define run_unit_tests
	cd rust && \
		export Python3_ROOT_DIR="$$($(PYTHON) -c 'import pathlib, sys; print(pathlib.Path(sys.base_prefix).resolve())')" && \
		export Python3_EXECUTABLE="$$($(PYTHON) -c 'import sys; print(sys.executable)')" && \
		cargo test --locked $(1) --no-default-features && \
		cargo test --locked $(1) --all-features
endef

# An explicit Cargo target makes PyO3 treat this as a cross build and omit
# libpython. The extension must keep that behavior, but the cargo test
# executable itself needs the development interpreter's embed link flags.
define run_sanitized_unit_tests
	cd rust && \
		unset LD_LIBRARY_PATH && \
		python_link_flags="" && \
		for flag in $$($(PYTHON)-config --ldflags --embed); do \
			python_link_flags="$$python_link_flags -Clink-arg=$$flag"; \
		done && \
		export CC=clang CXX=clang++ && \
		export RUSTUP_TOOLCHAIN="$(RUST_NIGHTLY)" && \
		export CARGO_BUILD_TARGET="$(RUST_TARGET)" && \
		export RUSTFLAGS="-Zsanitizer=$(1) -Cforce-frame-pointers=yes $$python_link_flags" && \
		export PYROSCOPE_SANITIZER="$(1)" && \
		export Python3_ROOT_DIR="$$($(PYTHON) -c 'import pathlib, sys; print(pathlib.Path(sys.base_prefix).resolve())')" && \
		export Python3_EXECUTABLE="$$($(PYTHON) -c 'import sys; print(sys.executable)')" && \
		cargo test -Zbuild-std=std --target "$(RUST_TARGET)" --locked --no-default-features && \
		cargo test -Zbuild-std=std --target "$(RUST_TARGET)" --locked --all-features
endef

.PHONY: test/unit/debug
test/unit/debug:
	$(call run_unit_tests,)

.PHONY: test/unit/release
test/unit/release:
	$(call run_unit_tests,--release)

.PHONY: test/unit/asan
test/unit/asan:
	$(call run_sanitized_unit_tests,address)

.PHONY: test/unit/tsan
test/unit/tsan:
	$(call run_sanitized_unit_tests,thread)

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
