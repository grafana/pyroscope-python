# Vendored from dd-trace-py

Code under `ddtrace/` is vendored from
[DataDog/dd-trace-py](https://github.com/DataDog/dd-trace-py) and kept at its
**upstream paths**, so a file here and the same file there can be compared
directly. Everything else in this repository is ours; in particular
`cpp/Pyroscope.h`, `cpp/CMakeLists.txt` and `cpp/BundleStaticLibrary.cmake` are
Pyroscope's, and they are what compile the vendored sources.

dd-trace-py is dual licensed Apache-2.0 OR BSD-3-Clause, compatible with this
repository's Apache-2.0.

## Components

| Component | Pinned at | Paths |
|---|---|---|
| memalloc (memory profiler) | `v4.11.1`, commit `823648d32ed5d2ab843be193a3abcc4d63feb1f7` | `ddtrace/profiling/collector/_memalloc*`, `ddtrace/profiling/collector/_pymacro.h`, `ddtrace/internal/datadog/profiling/profiling_helpers/` |

## The mirror branch

`vendor/dd` carries the upstream history of these files: pure upstream code, no
patches of ours. It is merged into main, so:

```sh
git diff vendor/dd HEAD -- ddtrace/   # exactly our patches, nothing else
```

Each commit on it keeps its upstream author, author date and message, and adds
`Vendor-Component` and `Upstream-Commit` trailers. It is re-signed because this
org requires signed commits and upstream's are not.

The branch is append-only and protected: it cannot be force-pushed or deleted,
and nothing is pushed to it directly. Imports land on an ordinary branch and
reach the mirror through a pull request, like any other change.

## Maintaining this

```sh
cd tools/vendor-ddtrace
go run . verify -component memalloc               # list our patches
go run . verify -component memalloc -ref v4.11.1  # and check the mirror is that ref
go run . sync   -component memalloc -ref <newer> -import-branch import/dd-memalloc-<newer>
```

`sync` replays the upstream commits since the last import, so an upgrade is an
ordinary three-way merge that conflicts only in the files listed below. Review
the import into `vendor/dd` first, then merge the result into your branch.

**Merge those pull requests with a merge commit, never a squash.** A squash
drops the mirror parent, and with it the ancestry the next upgrade needs.

## What was not taken

`memalloc.py` and `_memalloc.pyi` (the Python collector and its stubs, replaced
by `rust/src/memory.rs` driving a C ABI), upstream's `CMakeLists.txt`, upstream's
tests, and the whole `ddup`/libdatadog path, which owns pprof encoding and
upload in dd-trace-py and cannot coexist with the Rust one here.

## Our patches to memalloc

Nine of the fourteen files are upstream byte for byte. The five below are
patched, and every hunk is marked in place with a `Pyroscope patch:` comment.

### `_memalloc.cpp`

- Drops `#include "ddup_interface.hpp"` and the `ddup_start()` call: the Rust
  profile builder owns initialization, and there is no libdatadog here.
- Drops the `pthread_atfork` registration of `memalloc_heap_postfork_child`.
  Rust registers it through `os.register_at_fork`, so keeping both would run the
  child handler twice.
- Replaces the CPython extension module (`memalloc_start`/`memalloc_stop` as
  `PyCFunction`s, `PyMethodDef module_methods[]`, `PyInit__memalloc`) with a
  typed C ABI: `extern "C" int memalloc_start(uint16_t max_nframe, uint64_t
  heap_sample_size, bool enable_mem_domain)`, `extern "C" void memalloc_stop()`
  and `extern "C" void memalloc_heap_py()`, all idempotent, called from
  `rust/src/memory.rs`.
- On stop, deliberately leaves `g_saved_alloc_mem_pub` pointing at the saved
  allocator instead of nulling it, so a `memalloc_free_mem` still in flight
  cannot take the early exit and leak the block. The ordering argument is in the
  comment at that line.

### `_memalloc_heap.cpp`

- Uses `absl::flat_hash_map` unconditionally. Upstream falls back to
  `std::unordered_map` outside `NDEBUG`; our CMake always provides Abseil, and
  one map implementation means debug and release builds behave alike.
- Two undefined-behaviour fixes in `next_sample_size_no_cpython`, **worth
  sending upstream**: widen `sample_size` to `double` before `+ 1` so
  `UINT32_MAX` cannot wrap the rate to infinity, and clamp the draw before
  casting it to `uint32_t`, since the exponential distribution is unbounded and
  a draw above 2^32 makes the cast undefined.
- Drops `Datadog::Sample::profile_borrow().stats().set_heap_tracker_size()`:
  there is no Datadog profile-state object on this side.
- Passes the sampling-scaled allocation count to `push_heap()` so live samples
  report `inuse_objects` with the same estimate `alloc_objects` used.

### `_memalloc_tb.cpp`, `_memalloc_tb.h`

`Datadog::Sample` becomes `Pyroscope::Sample` (`sample.hpp` becomes
`Pyroscope.h`), and the constructor drops the Datadog sample-type flags, which
the Rust profile builder does not use. `_memalloc_tb.h` also wraps the
`memalloc_heap_postfork_child` declaration in `extern "C"`.

### `_memalloc_heap.h`

Wraps the `memalloc_heap_postfork_child` declaration in `extern "C"` so Rust can
call it.
