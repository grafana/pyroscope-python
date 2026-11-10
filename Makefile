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
ASAN_BUILD_DIR ?= $(CURDIR)/.tmp/cpython-asan/$(subst .,,$(ASAN_PYTHON_REF))
ASAN_CC ?= clang-18
ASAN_CXX ?= clang++-18

asan/clone: $(ASAN_BUILD_DIR)/src/.cloned

$(ASAN_BUILD_DIR)/src/.cloned:
	mkdir -p "$(ASAN_BUILD_DIR)"
	git clone --depth 1 --branch "$(ASAN_PYTHON_REF)" https://github.com/python/cpython.git "$(ASAN_BUILD_DIR)/src"
	touch "$@"

asan/python: $(ASAN_BUILD_DIR)/install/.built

$(ASAN_BUILD_DIR)/install/.built: $(ASAN_BUILD_DIR)/src/.cloned
	export CC="$(ASAN_CC)" CXX="$(ASAN_CXX)" ASAN_OPTIONS=detect_leaks=0; \
	export LD_LIBRARY_PATH="$(ASAN_BUILD_DIR)/install/lib$${LD_LIBRARY_PATH:+:$$LD_LIBRARY_PATH}"; \
	cd "$(ASAN_BUILD_DIR)/src" && \
	./configure --prefix="$(ASAN_BUILD_DIR)/install" --enable-shared --with-address-sanitizer --without-pymalloc --with-ensurepip=install && \
	$(MAKE) -j"$$(getconf _NPROCESSORS_ONLN)" && \
	$(MAKE) install
	touch "$@"

.PHONY: asan/test
asan/test: asan/python
	CC="$(ASAN_CC)" \
		CXX="$(ASAN_CXX)" \
		PROFILER='$(PROFILER)' \
		RUN='$(RUN)' \
		bash scripts/asan-test.sh "$(ASAN_BUILD_DIR)"

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
