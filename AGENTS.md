# pyroscope-python

Python profiling agent. Ships as a single `pyroscope._native` extension: a Rust
core (`rust/`) that owns session management, pprof encoding and upload, plus
C++ profilers from dd-trace-py (the `dd-trace-py/` submodule) that Rust drives over a cbindgen FFI boundary.

**Every profiler here runs inside the process it profiles.** There is no
out-of-process agent and nothing attaches to a foreign pid. Where a sampler
reads memory with `process_vm_readv` / `mach_vm_read_overwrite`, the target is
our own process: the syscall buys a fault-free read of another *thread* without
taking the GIL, not access across a process boundary.

## What this branch is doing

**Vendoring dd-trace-py's CPU profiler into Pyroscope**, reimplementing every
piece of it that depends on libdatadog.

dd-trace-py's CPU profiler is an echion-based sampling stack profiler: a
dedicated sampling thread walks other threads' Python frames out-of-band,
reading this process's own CPython internals through `process_vm_readv` /
`mach_vm_read_overwrite` rather than holding the GIL. It handles asyncio tasks,
greenlets, uvloop, and native frames via `sys.monitoring`. That machinery is
what we want; its transport is not.

This repeats what was already done for dd-trace-py's **memory** profiler
(`dd-trace-py/ddtrace/profiling/collector/`), which is vendored, wired up and shipping. The CPU profiler
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

## Vendor provenance

The C++ lives in the `dd-trace-py/` submodule: grafana/dd-trace-py, branch `CPU`. That fork is upstream DataDog/dd-trace-py pruned to its profiling sources, with every Pyroscope patch as a commit on top; its `AGENTS.md` lists the patched files and how to sync from upstream. Change the C++ there, then bump the gitlink here.

- `CPU` is branched from `pyroscope-memalloc` ([#2](https://github.com/grafana/dd-trace-py/pull/2)), on upstream `e2c6c051f991ced028e450874ea8b4b7d49cc7e0` (`main`, 2026-09-29).
- In the paths below, `profiling/` is `dd-trace-py/ddtrace/internal/datadog/profiling/` and `collector/` is `dd-trace-py/ddtrace/profiling/collector/`.

`profiling/dd_wrapper/` is mostly our shims. `clock.hpp`, `constants.hpp`, `defer.hpp` and `scope.hpp` are verbatim upstream.

## Layout

| Path | Origin | Notes |
|---|---|---|
| `profiling/stack/` | upstream | The echion CPU sampler. Runs and produces data. Upstream's `stack/src/stack.cpp` (the `_stack` CPython module) is kept but **not built**, replaced by `dd-trace-py/pyroscope/stack_ffi.cpp`. |
| `profiling/dd_wrapper/` | upstream | Upstream's shared C++ layer. Mostly our shims; a few verbatim copies. |
| `collector/_memalloc*` | upstream | Memory profiler. Done and shipping. |
| `dd-trace-py/pyroscope/Pyroscope.h` | ours | The central shim: `Sample`, `intern_utf8_string`, `string_id`, `ProfilerStats`, `ProfileBorrow`. Shared by both profilers. |
| `profiling/profiling_helpers/` | upstream | Version-gated CPython frame accessors. |
| `dd-trace-py/CMakeLists.txt` | ours | Builds both profilers into the static library `rust/build.rs` links. |
| `rust/src/encode/` | ours | pprof builder + the process-wide string interner the C++ side interns into. |
| `rust/src/stack/` | ours | The cpu/wall accumulator and dump path behind `PprofBuilderType::CpuWall`; `sampler.rs` wraps the C++ FFI, `threads.rs`, `asyncio.rs` and `faulthandler.rs` patch the Python modules. |

`profiling/` is on the include path, so upstream's `#include
"dd_wrapper/include/..."` lines resolve unchanged. **Preserving upstream paths
and include spellings is deliberate** -- it keeps the diff against upstream
small and makes the next vendor sync tractable. Prefer adding a shim at the
upstream path over editing a vendored source.

## Conventions

- **Write no comments in the Rust and C++ we author.** Not a header block, not
  a rationale, not a one-liner above a tricky expression. What a reader cannot
  derive from the code goes in `stack_todo.md`, `stack_known_bugs.md` or
  `stack_scope.md`, each citing a symbol. If a comment feels necessary, the
  answer is clearer code, a test that encodes the invariant, or a doc entry.
- Vendored files keep upstream's comments verbatim. Do not strip them and do
  not add to them -- the diff against upstream is the point.
- Two markers are the only exception, because they are a grep index for the
  next vendor sync rather than explanation. Both are one line, no prose:
  `// Pyroscope patch:` naming the difference where a shim's behaviour departs
  from the upstream symbol it stands in for, and `TODO(Pyroscope):` at an
  unfinished site, whose explanation lives in `stack_todo.md`.
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
  load-bearing, and `dd-trace-py/CMakeLists.txt` documents flags that must *not* be
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

The C++ compiles and archives warning-free for Python 3.10-3.14 under Apple
clang 21, clang 22 on `manylinux2014` and `musllinux_1_2`, and gcc
10.2/13/14. There is no cargo feature gating the
C++ half any more -- it is always built. No profiler here supports
free-threaded interpreters, py-spy and memalloc included, so `setup.py` and
`dd-trace-py/CMakeLists.txt` reject `Py_GIL_DISABLED` at configure time.

**The sampler runs.** `cpu_implementation=ProfilerImplementation.Stack` drives
it end to end: `crate::stack::start` configures and starts
`Datadog::Sampler` through `dd-trace-py/pyroscope/stack_ffi.cpp`, patches `threading`
to populate echion's thread info map, and the accumulated cpu+wall samples
upload as `process_cpu` alongside the memory profile. Verified against a live
server by `TestPythonStackProfilerOnCPU` in `integration-test/`.

Every sampler setting except `cpu_implementation` lives on its own entry point,
`pyroscope.configure_cpu_profiler`, because none of them is re-appliable:
`stack::set_options` takes the first call per process and refuses the rest,
shutdown or not, and a session that starts the sampler fixes them too. Fast copy
is on by default; adaptive sampling (`adaptive_sampling`) and asyncio task
unwinding (`async_tracking`, which covers uvloop too) are off, so the rest of
the `adaptive_*` group is inert too. `async_tracking` is the only one that stops
at Rust: `rust/src/stack/asyncio.rs` patches `asyncio` and `uvloop` the
way `threads.rs` patches `threading`, and never touches
`pyroscope_stack_configure`.

Unlike upstream, fast copy's SIGSEGV/SIGBUS handlers and the faulthandler patch
install only when it is on, from `stack::start` rather than at import.

Read all three tracking docs before picking up CPU profiler work:
`stack_scope.md` for what this iteration is not doing, `stack_todo.md` for what
is left, and `stack_known_bugs.md` before "fixing" something that looks broken.

## Build and verify

The C++ half is driven by CMake from `rust/build.rs`, which needs the target
interpreter passed down (normally by `setup.py`):

```sh
# fast loop on the C++ static library alone
cmake -S dd-trace-py -B /tmp/ddbuild -G Ninja \
  -DPython3_EXECUTABLE=$(which python3) -DPython3_FIND_STRATEGY=LOCATION \
  -DPYROSCOPE_FFI_INCLUDE_DIR=$PWD/rust/include
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
- `profiling/stack` is heavily `PY_VERSION_HEX`-gated, but for iteration test Python
  3.13 only; the full version matrix is left to CI (see `stack_todo.md`).
- `PL_LINUX` selects different code in `vm.cc` and `danger.cc`, so macOS alone
  misses real breakage. Build on Linux too (`ssh orb`, where these sources are
  mounted at identical paths).
- The wheels are built with clang 22, not gcc, installed by
  `manylinux-install-clang` in `manylinux2014` and `musllinux_1_2`.
- `-Werror` is on for `pyroscope_memalloc` and `pyroscope_stack`, and only
  those two: abseil is not warning-free. There is no switch to turn it off.
- Regenerate the FFI header with `make ffi/python/header` after touching the
  `ffi` module, and add new exports to `rust/cbindgen.toml`.
