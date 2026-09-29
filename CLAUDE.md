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
| `cpp/stack/` | upstream `profiling/stack/` | The echion CPU sampler. `echion/` subdir kept; `stack_v2` flattened to `stack`. Runs and produces data. Upstream's `stack/src/stack.cpp` (the `_stack` CPython module) is **deleted**, replaced by `cpp/pyroscope/stack_ffi.cpp`. |
| `cpp/dd_wrapper/` | upstream `profiling/dd_wrapper/` | Upstream's shared C++ layer. Mostly our shims; a few verbatim copies. |
| `cpp/memalloc/` | upstream `profiling/memalloc/` | Memory profiler. Done and shipping. |
| `cpp/pyroscope/Pyroscope.h` | ours | The central shim: `Sample`, `intern_string`, `string_id`, `ProfilerStats`, `ProfileBorrow`. Shared by both profilers. |
| `cpp/profiling_helpers/` | upstream | Version-gated CPython frame accessors. |
| `rust/src/encode/` | ours | pprof builder + the process-wide string interner the C++ side interns into. |
| `rust/src/stack.rs` | ours | The cpu/wall accumulator and dump path behind `PprofBuilderType::CpuWall`. |

`cpp/` is on the include path, so upstream's `#include
"dd_wrapper/include/..."` lines resolve unchanged. **Preserving upstream paths
and include spellings is deliberate** -- it keeps the diff against upstream
small and makes the next vendor sync tractable. Prefer adding a shim at the
upstream path over editing a vendored source.

## Conventions

- Where a shim's behaviour differs from the upstream symbol it stands in for,
  mark it `// Pyroscope patch:` so a vendor sync can find it. Keep it to the
  difference itself;
- Unfinished work gets a `TODO(Pyroscope):` at the site, and an entry in
  `stack_todo.md`. Put the explanation in `stack_todo.md`, not at the site.
- A defect we decide to live with goes in `stack_known_bugs.md`, with the reason
  we are not fixing it. Do not re-file it as work in `stack_todo.md`.
- We do not fix upstream's bugs in this integration, and we do not report them
  upstream either. An inherited defect gets documented in
  `stack_known_bugs.md` and left alone; it never becomes work in
  `stack_todo.md`.
- Something we have decided not to do goes in `stack_scope.md`, and stays there.
  It is not work: do not file it in `stack_todo.md`, and do not write up how it
  would be implemented.
- All three docs are lists, not prose: an entry is a sentence or two, three
  lines at most, and cites a symbol rather than a line number. A finished item
  is deleted -- git history is the record. Write what a reader cannot derive
  from the code, and nothing else.
- Do not "fix" a vendored oddity without checking upstream first -- several are
  load-bearing, and `cpp/CMakeLists.txt` documents flags that must *not* be
  restored on a sync.

## Locking rules

Three lock-like things are in play -- the GIL, `ffikit::STATE`, and the
`encode::interner` / profile-builder mutexes -- plus echion's
`thread_info_map_lock` on the C++ side. The sampling thread runs GIL-free by
design, so every ordering below is a real interleaving, not a theoretical one.

- **Never hold `ffikit::STATE` across anything that needs the GIL.** A thread
  entering `run`/`stop` blocks on that mutex *while holding the GIL*, so a
  holder that then waits for the GIL deadlocks both. This already happened:
  `stack::start` reaches `PyModule::from_code`, whose module body lets CPython
  drop the GIL, and two concurrent `configure()` calls wedged permanently.
  `run` and `stop` therefore claim a `State::Busy` marker, release the lock,
  and only then start or stop profilers.
- **Never block on a C++ profiler from under the GIL.** `stack::stop` wraps
  `Sampler::stop()` in `py.detach`: that call waits up to 3 s for the sampling
  thread, which interns strings, and `memory::dump_pprof` holds the interner
  lock while attached to Python. The same applies to
  `register_thread` / `unregister_thread`, which take echion's
  `thread_info_map_lock`.
- **Interner before profile builder, never the reverse.** Spelled out on
  `interner::clear`; `memory::dump_pprof` and `stack::dump_pprof` both need
  both locks.
- **Stop every producer before clearing the interner.** `stop_profilers` runs
  `memory::stop` and `stack::stop` (which joins the sampling thread) before
  `interner::clear`, or a live sampler re-warms caches with indices that are
  about to become stale.

## Status and open work

`cpp/` compiles and archives warning-free for Python 3.10-3.14 under Apple
clang 21, clang 21 on `manylinux_2_28`, clang 20 on `musllinux_1_2`, and gcc
10.2/13/14. There is no cargo feature gating the
C++ half any more -- it is always built. No profiler here supports
free-threaded interpreters, py-spy and memalloc included, so `setup.py` and
`cpp/CMakeLists.txt` reject `Py_GIL_DISABLED` at configure time.

**The sampler runs.** `cpu_implementation=ProfilerImplementation.Stack` drives
it end to end: `crate::stack::start` configures and starts
`Datadog::Sampler` through `cpp/pyroscope/stack_ffi.cpp`, patches `threading`
to populate echion's thread info map, and the accumulated cpu+wall samples
upload as `process_cpu` alongside the memory profile. Verified against a live
server with `scripts/tests/test_stack_cpu.py`.

Adaptive sampling (`cpu_adaptive_sampling`) and fast copy (`cpu_fast_copy`) are
off by default but reach the sampler when asked for. Unlike upstream, fast
copy's SIGSEGV/SIGBUS handlers and the faulthandler patch install only when it
is on, from `configure()` rather than at import, and the first `configure()`
fixes the choice for the process.

Read all three tracking docs before picking up CPU profiler work:
`stack_scope.md` for what this iteration is not doing, `stack_todo.md` for what
is left, and `stack_known_bugs.md` before "fixing" something that looks broken.

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
  Python3_EXECUTABLE=$(which python3) cargo test

# full wheel
python3 -m build --wheel
```

Caveats worth knowing before trusting a green build:

- Only what registration reaches is linked into the final `.so`; the rest of
  the CPU sampler is still dropped. Verify against the static archive, and note
  that its C++ symbols are hidden, so inspect the `.so` with `nm -a`, not `-g`.
- `cpp/stack` is heavily `PY_VERSION_HEX`-gated, but for iteration test Python
  3.13 only; the full version matrix is left to CI (see `stack_todo.md`).
- `PL_LINUX` selects different code in `vm.cc` and `danger.cc`, so macOS alone
  misses real breakage. Build on Linux too (`ssh orb`, where these sources are
  mounted at identical paths).
- The wheels are built with clang, not gcc: clang 21 from AlmaLinux 8 appstream
  in `manylinux_2_28`, clang 20 from `apk add clang20` in `musllinux_1_2`. On
  glibc, `docker/wheels.sh` pins clang's gcc installation so its libstdc++
  headers match the `libstdc++.a` the rust link step consumes.
- `-Werror` is on for `pyroscope_memalloc` and `pyroscope_stack`, and only
  those two: abseil is not warning-free. There is no switch to turn it off.
- Regenerate the FFI header with `make ffi/python/header` after touching the
  `ffi` module, and add new exports to `rust/cbindgen.toml`.
