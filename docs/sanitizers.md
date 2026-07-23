# Native build configurations and sanitizers

The native extension is tested in four configurations on Linux x86_64:

- `debug`: Cargo dev profile and CMake Debug
- `release`: Cargo release profile and CMake Release
- `asan`: debug profile instrumented with AddressSanitizer
- `tsan`: debug profile instrumented with ThreadSanitizer

Sanitized builds are test artifacts and must never be published. The regular
wheel compatibility matrix remains responsible for release-wheel coverage
across supported Python versions, architectures, and libc implementations.

## Why sanitized CPython is required

The Rust extension, C++ memory profiler, Abseil, and CPython must use one
sanitizer runtime:

- TSan needs CPython instrumentation to observe synchronization performed by
  the interpreter.
- ASan needs CPython instrumentation and `PYTHONMALLOC=malloc` to observe
  allocations that would otherwise stay inside pymalloc arenas.
- The extension is built with Rust's `-Zexternal-clangrt`, so its sanitizer
  symbols resolve from the runtime loaded by CPython at process startup.

The extension intentionally does not link `libpython`. Linking a static
sanitized `libpython.a` would bundle a second CPython runtime into the
extension and invalidate the test.

TSan's standard `--with-thread-sanitizer` configure option requires CPython
3.13 or newer. CI uses a pinned CPython 3.13 release for both sanitizers.

## Local native builds

Debug and release builds can use a regular Python environment:

```sh
python3 -m pip install build setuptools setuptools-rust wheel
make build/debug
make test/unit/debug

make build/release
make test/unit/release
```

Sanitizer builds require Linux x86_64, Clang, CMake, and a nightly Rust
toolchain with `rust-src`. Build CPython with the matching sanitizer and
prefer a shared `libpython`, which makes the Clang runtime available to loaded
extensions:

```sh
# AddressSanitizer
CC=clang CXX=clang++ ./configure \
  --prefix="$HOME/cpython-asan" \
  --enable-shared \
  --with-address-sanitizer
make -j"$(nproc)"
make install

# ThreadSanitizer (CPython 3.13+)
CC=clang CXX=clang++ ./configure \
  --prefix="$HOME/cpython-tsan" \
  --enable-shared \
  --with-thread-sanitizer
make -j"$(nproc)"
make install
```

Create a virtual environment from that interpreter, install the Python build
requirements, and select the same Rust nightly used by the build:

```sh
rustup toolchain install nightly --profile minimal --component rust-src

export LD_LIBRARY_PATH="$HOME/cpython-asan/lib"
~/cpython-asan/bin/python3 -m venv ~/venv-asan
. ~/venv-asan/bin/activate
python -m pip install build setuptools setuptools-rust wheel
make build/asan
make test/unit/asan PYTHON=/usr/bin/python3
```

Use `build/tsan` with the TSan interpreter, then run `test/unit/tsan` with a
normal development Python through the `PYTHON` override shown above. Override
`RUST_NIGHTLY` and `RUST_TARGET` when a non-default nightly or target is
required.

The unit-test sanitizer executable owns Rust's sanitizer runtime, so unit tests
should use a normal development Python through `PYTHON=/path/to/python3`.
The extension itself must be imported only by the matching sanitized Python.

## Runtime options

Recommended fail-fast settings are:

```sh
ASAN_OPTIONS="detect_leaks=0:halt_on_error=1:allocator_may_return_null=1:handle_segv=0"
PYTHONMALLOC=malloc

TSAN_OPTIONS="halt_on_error=1:exitcode=66:handle_segv=0:suppressions=/path/to/cpython/Tools/tsan/suppressions.txt"
```

Leak detection is disabled because CPython retains interpreter-level
allocations at shutdown. Do not blanket-suppress reports containing CPython
frames: races and memory errors crossing the extension/interpreter boundary
are relevant.

On Linux hosts with high ASLR entropy, TSan may fail to reserve its shadow
address range. Match CPython CI before running TSan:

```sh
sudo sysctl -w vm.mmap_rnd_bits=28
```

## Reproducing the CI image

The parameterized test image builds the correct interpreter, extension wheel,
and toolchains:

```sh
docker buildx build \
  --platform linux/amd64 \
  --file docker/test.Dockerfile \
  --build-arg BUILD_CONFIG=asan \
  --load \
  --tag pyroscope-python-test:asan \
  .

docker run --rm pyroscope-python-test:asan \
  make test/unit/asan PYTHON=/usr/local/bin/python3
```

Valid `BUILD_CONFIG` values are `debug`, `release`, `asan`, and `tsan`. The CI
workflow extracts `/wheels`, checks that CPython symbols were not bundled,
verifies sanitizer references, and runs the complete Go integration suite
using the same image.

The focused sanitizer matrix is Linux x86_64 only. macOS and Linux arm64 are
deferred until the initial lanes are stable; this is a current CI scope
decision, not a claim that LLVM sanitizers cannot support those platforms.
