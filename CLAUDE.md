# pyroscope-python

Python profiling agent. Ships as a single `pyroscope._native` extension: a Rust
core (`rust/`) that owns session management, pprof encoding and upload, plus
vendored C++ profilers (`cpp/`) that Rust drives over a cbindgen FFI boundary.

## What this branch is doing

**Vendoring dd-trace-py's CPU profiler into Pyroscope**, reimplementing every
piece of it that depends on libdatadog.

dd-trace-py's CPU profiler is an echion-based sampling stack profiler: a
dedicated sampling thread walks other threads' Python frames out-of-band,
reading CPython internals through `process_vm_readv` / `mach_vm_read_overwrite`
rather than holding the GIL. It handles asyncio tasks, greenlets, uvloop, and
native frames via `sys.monitoring`. That machinery is what we want; its
transport is not.

This repeats what was already done for dd-trace-py's **memory** profiler
(`cpp/memalloc/`), which is vendored, wired up and shipping. The CPU profiler
is the same exercise at roughly ten times the size.

The hard constraint is the same in both cases: **upstream writes profiles
through libdatadog and uploads them to Datadog's backend; we do neither.**
Samples must land in our own Rust pprof encoder instead. So the port splits
every upstream file into one of three buckets:

1. **Libdatadog-free** -- copy verbatim, keep upstream's path and filename.
2. **Libdatadog-entangled** -- replace with a Pyroscope shim that keeps
   upstream's names, signatures and call shapes so the vendored callers compile
   unmodified.
3. **Pure transport** (uploader, Profiles Dictionary, endpoint counts, code
   provenance) -- do not port at all.

Upstream checkout for reference: `~/dd/dd-trace-py`, under
`ddtrace/internal/datadog/profiling/`.

## Layout

| Path | Origin | Notes |
|---|---|---|
| `cpp/stack/` | upstream `profiling/stack/` | The echion CPU sampler. `echion/` subdir kept; `stack_v2` flattened to `stack`. Compiles, produces no data yet. |
| `cpp/dd_wrapper/` | upstream `profiling/dd_wrapper/` | Upstream's shared C++ layer. Mostly our shims; a few verbatim copies. |
| `cpp/memalloc/` | upstream `profiling/memalloc/` | Memory profiler. Done and shipping. |
| `cpp/pyroscope/Pyroscope.h` | ours | The central shim: `Sample`, `intern_string`, `string_id`, `ProfilerStats`, `ProfileBorrow`. Shared by both profilers. |
| `cpp/profiling_helpers/` | upstream | Version-gated CPython frame accessors. |
| `rust/src/encode/` | ours | pprof builder + the process-wide string interner the C++ side interns into. |

`cpp/` is on the include path, so upstream's `#include
"dd_wrapper/include/..."` lines resolve unchanged. **Preserving upstream paths
and include spellings is deliberate** -- it keeps the diff against upstream
small and makes the next vendor sync tractable. Prefer adding a shim at the
upstream path over editing a vendored source.

## Conventions

- **Every deviation from upstream gets a `// Pyroscope patch:` banner** naming
  the upstream symbol it replaces, how ours differs, and what callers must
  therefore *not* do. These are the port's real documentation; there is no
  design doc. See `cpp/pyroscope/Pyroscope.h` for the house style.
- Rationale lives in code comments, not commit bodies.
- Unfinished work gets a `TODO(Pyroscope):` at the site, and an entry in
  `stack_todo.md`.
- Do not "fix" a vendored oddity without checking upstream first -- several are
  load-bearing, and `cpp/CMakeLists.txt` documents flags that must *not* be
  restored on a sync.

## Status and open work

`cpp/stack` compiles and archives warning-free on macOS/clang for Python
3.11-3.14 and on Linux/gcc 13 for 3.12. The sampler walks stacks correctly and
then **discards every sample**: no CPU push path to Rust exists yet, and nothing
in Python imports the extension.

`stack_todo.md` is the tracking doc -- blocking work, gaps the port opened,
free-threaded-build questions, and the TODOs inherited from upstream, kept
separate so they are not confused with ours. Read it before picking up CPU
profiler work.

## Build and verify

The C++ half is driven by CMake from `rust/build.rs`, which needs the target
interpreter passed down (normally by `setup.py`):

```sh
# fast loop on the C++ static library alone
cmake -S cpp -B /tmp/ddbuild -G Ninja \
  -DPython3_EXECUTABLE=$(which python3) -DPython3_FIND_STRATEGY=LOCATION
cmake --build /tmp/ddbuild

# Rust, including the C++ build
cd rust && Python3_ROOT_DIR=$(python3 -c 'import sys,pathlib;print(pathlib.Path(sys.base_prefix).resolve())') \
  Python3_EXECUTABLE=$(which python3) cargo test --features memory

# full wheel
python3 -m build --wheel
```

Caveats worth knowing before trusting a green build:

- Nothing references the CPU sampler yet, so **the linker drops it from the
  final `.so`**. Verify against the static archive, not the extension module.
- `cpp/stack` is heavily `PY_VERSION_HEX`-gated; a single-version build proves
  little.
- `PL_LINUX` selects different code in `vm.cc` and `danger.cc`, so macOS alone
  misses real breakage. Build on Linux too (`ssh orb`, where these sources are
  mounted at identical paths).
- `-Werror` is intentionally off; treat any new warning as a defect anyway.
- Regenerate the FFI header with `make ffi/python/header` after touching the
  `ffi` module, and add new exports to `rust/cbindgen.toml`.
