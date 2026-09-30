# CPU stack profiler: PRs to submit ahead of it

Four PRs that main can take with no `cpp/stack` present. Landing them takes the
sampler patch from ~9.2k lines / 87 files to ~8.2k / ~60, and the non-vendored
part a reviewer must judge from ~3.4k to ~2.4k.

Assemble each from the branch tip (`git checkout feat/dd_cpu_attempt3 --
<paths>`, then strip the sampler-only parts), not by cherry-picking: several
commit pairs are net-zero (`95c9fb6`, `0b40a0d`, `9ab2042` delete shims added
earlier; `cebfee0` adds a `ci.yml` and `test_free_threaded.py` that `2844aca`
deletes). Diff against the branch base `22c9c2f`, not local `main`, which is
behind it.

## A. C++ layout and build hygiene (~145 lines)

`2a9c1fd`, `ed1bda3`, `e2ad430`, `bf7dbfb`, and `45158ce` minus every
`pyroscope_stack` line.

- `cpp/*` -> `cpp/memalloc/`, `cpp/Pyroscope.h` -> `cpp/pyroscope/`, and
  `_memalloc_frame.h` includes `profiling_helpers/...` without the `../`.
- cmake: `pyroscope_common` INTERFACE library, `MEMALLOC_SOURCES` as the
  `pyroscope_memalloc` OBJECT library. The sampler PR then only *adds* a target.
- cmake: drop the global `_POSIX_C_SOURCE`/`_DARWIN_C_SOURCE`, keeping the
  `// Pyroscope patch:` note. macOS fix: they hide the BSD types abseil needs.
- `build.rs`: `NATIVE_SOURCES` -> recursive `emit_rerun_for_dir`. After the move
  every entry names a dead path, so C++ edits trigger no rebuild.

Verify: cmake + `cargo test` + `python3 -m build --wheel`, on macOS and
`ssh orb`; touch a file under `cpp/memalloc/` and confirm cargo rebuilds.

## B. Always build the C++ profiler, reject free-threaded CPython (~90 lines)

`cebfee0` + `2844aca` + `3106d34` + `d342128`. User-visible: a 3.14t build goes
from a py-spy-only wheel to a build error. Say that in the PR body.

- Delete `[features]` from `rust/Cargo.toml` and the `#[cfg(feature =
  "memory")]` arms from `memory.rs`.
- `build.rs`: drop the feature early-return and `reject_free_threaded()`; no
  more spawning python.
- cmake: `check_symbol_exists(Py_GIL_DISABLED ...)` -> `FATAL_ERROR`. Keep the
  `unset(... CACHE)` or a reconfigured build dir keeps the old answer.
- `setup.py`: `raise SystemExit` citing
  [#163](https://github.com/grafana/pyroscope-python/issues/163).
- `ci-rust.yml`: one `--locked` clippy/test run, and `Python3_ROOT_DIR` /
  `Python3_EXECUTABLE` exported for clippy and miri.

## C. Generic pprof builder, shared interner, one FFI entry point (~650 lines)

The non-cpu/wall half of `31c489a`, `32f9649`, `9907aaf`, `b326d9e`.

- `pprof.rs`: `ProfileKind` / `FfiProfileKind`, `PProfBuilder<K>`,
  `MemoryProfile` + `PySpyProfile`, `set_profile_type`, `ffi_samples`,
  `add_stacktrace` under `impl PProfBuilder<PySpyProfile>`. `PprofBuilderType`
  with `Memory` and `Cpu` only. `FFIHeapSampleValues` -> `FFISampleValues`,
  `FFISample` gone. Leave `CpuWallProfile`, the `cpu_time`/`wall_time` slots and
  their three tests behind.
- New `encode/interner.rs`: the process-wide `LeakableMutex<StringTable>`,
  `pyroscope_string_table_intern_utf8`, `string_table()`, `clear()`,
  `postfork_child()`, the interner-before-builder lock order, its test.
- New `ffi.rs`: `pyroscope_push_sample` with the `Memory` and no-op `Cpu` arms.
- `memory.rs`: drop the local `STRING_TABLE` and both `pyroscope_memprof_*`
  exports; `PROFILE_BUILDER` becomes a `LeakableMutex`; `clear_state` ->
  `clear_samples`, no longer dropping strings.
- `ffikit::stop_profilers` (memory stop, then `interner::clear()`), called from
  `stop` and `at_fork_after_in_child`. **The change to flag in review:** the
  string table now clears at agent teardown, not in `memory::stop`, so
  `start()`'s rollback no longer invalidates ids.
- `Pyroscope.h`: `string_id`, `inline intern_utf8_string`, the `string_id`
  `push_frame` overload, `Sample(max_nframes, PprofBuilderType)` (see
  `_memalloc_tb.cpp:136`). `ProfilerStats`/`ProfileBorrow` stay behind.
- `cbindgen.toml` `[enum] prefix_with_name`; regenerate the header with `make
  ffi/python/header`, do not hand-edit.

Verify: `cargo test`, clippy, `cargo miri test --lib`, then
`scripts/tests/test_memory.py` against a live server -- this is memalloc's whole
sample path.

## D. ffikit locking fix + integration-test startup (~75 lines)

Two commits, unrelated but tiny.

- `ffikit::run`, from `7320ba2` minus `stack::start`: claim `State::Busy`, drop
  the lock, start, re-lock to store `Running` or roll back via `stop_profilers`
  + `Idle`. A thread blocking on `STATE` holds the GIL, so a holder that waits
  for the GIL wedges both. Verify with `test_concurrency.py`, `test_atexit.py`.
- `startPyroscope`: `-ingester.min-ready-duration=0s`,
  `-segment-writer.min-ready-duration=0s`,
  `-metastore.min-ready-duration=0s`, plus the pin-the-image TODO. Cuts fixed
  latency off every integration test.

## Stays in the sampler PR

`cpp/stack/**`, `cpp/dd_wrapper/**` (no memalloc source includes it),
`stack_ffi.cpp`, the `pyroscope_stack` cmake target, `rust/src/stack.rs`,
`CpuWallProfile` and `SamplerPauseResult`, every `stack::` arm in
`ffi.rs`/`ffikit.rs`/`lib.rs`, `pyroscope.rs`'s `process_cpu` dump,
`session.rs`'s `disabled_stack_config`, `ProfilerImplementation` and
`configure_cpu_profiler` with its twelve options, `test_stack_cpu.py`, `fork_workload.py`,
`sighandler_workload.py`, `restart_workload.py` and their Go tests, and the four
tracking docs.

For that PR's description: 29 of the 39 files under `cpp/stack/` are
byte-identical to upstream (3577 lines); only `include/sampler.hpp`,
`include/stack_renderer.hpp`, `src/sampler.cpp`, `src/stack_renderer.cpp`,
`echion/echion/{echion_sampler.h,vm.h,danger.h}` and
`src/echion/{stacks.cc,danger.cc,vm.cc}` differ, by ~400 lines. Re-derive with
`cmp` at submission time.
