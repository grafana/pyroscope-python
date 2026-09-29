# CPU stack profiler: outstanding work

Work list for `cpp/stack/` (dd-trace-py's echion-based CPU sampler) and the
`cpp/dd_wrapper/` shims under it. Defects we have decided to live with, and the
TODOs inherited from the vendored code, are in `stack_known_bugs.md` -- do not
re-file them here.

Format: one bullet per item, a sentence or two. A finished item becomes a struck
one-liner under `## Done` and loses its write-up. Cite a symbol, not a line
number.

## Open

- **No labels, and no thread or task information.** `push_threadinfo`,
  `push_task_name`, `push_span_id`, `push_local_root_span_id`,
  `push_trace_type` and `push_monotonic_ns` are no-ops. `PProfBuilder`'s FFI
  accumulator keys on the location-id vector alone and `flush_ffi_samples`
  hardcodes `label: vec![]`, so carrying them means re-keying the accumulator or
  pushing each sample directly, the shape `add_stacktrace` already uses.
- **No `samples/count` sample type.** `FFISampleValues` has no count slots and
  every call site passes a count of 1.
- **Native monitoring is not enabled.** `NativeCallRegistry::lookup` always
  returns `nullopt`. Restoring it needs upstream's
  `native_call_tracker.{hpp,cpp}`, `ProfilerState::postfork_child`, and
  `native_call_handler` / `start_native_monitoring` / `stop_native_monitoring`
  as `extern "C"` -- they lived in the deleted `stack/src/stack.cpp`,
  recoverable from upstream.
- **No samples after `configure()` -> `shutdown()` -> `configure()`.** The
  second run in one process uploads no cpu/wall samples for its canary tag.
  Cause not investigated. Wants an integration test:
  `integration-test/integration_test.go` covers the concurrent case
  (`testPythonConcurrentConfigureShutdown`) but not the sequential restart.
- **`ProfilerStats` and `ProfileBorrow` are inert** (`cpp/pyroscope/Pyroscope.h`).
  All 11 setters drop their argument. `set_string_table_count`,
  `add_sample_capture_cpu_time_us` and `add_copy_memory_error_count` are real
  health signals the sampler already computes. Making them real also means
  making `ProfileBorrow` hold a lock, which is what upstream's per-window
  `auto borrow = ...` batching is for.
- **Frame addresses are dropped.** `FFIFrame` has no address field and
  `add_location_mirror` hardcodes `address` and `mapping_id` to 0. Nothing of
  value is lost today.
- **`-Werror` is off** (`cpp/CMakeLists.txt`). `cpp/stack` is warning-free under
  Apple clang and gcc 13.3, but release wheels build on older
  manylinux/musllinux gcc whose diagnostics would hard-fail a release. Separate
  blocker for the memalloc half: `_memalloc.cpp` compares an unsigned
  `heap_sample_size < 0`.
- **The integration-test Pyroscope server is unpinned** (`startPyroscope`). The
  bare `grafana/pyroscope` tag means each machine runs whatever it last pulled.
  Pin the latest release and run `-architecture.storage=v2`: the default
  `v1-v2-dual` delays queryable data by ~60s past `/ready`, which is most of
  each CPU test's ~95s.
- **Fix the fast-copy warmup handler-swap gap**, and file it upstream and here.
  Gate `uninstall_segv_handler` / `reinstall_segv_handler` on "fast copy
  requested and `safe_memcpy_initialized`" rather than on `fast_copy_active`;
  `enable_during_warmup_falls_back` pins today's behaviour, so the fix flips a
  test. See `stack_known_bugs.md`.
- **Free-threaded builds are refused, not degraded.** `setup.py` raises and
  `cpp/CMakeLists.txt` reads `Py_GIL_DISABLED` out of the `pyconfig.h` it is
  about to compile against, so there is no `cp314t` wheel. py-spy cannot attach
  at all: `get_gil_threadid` is called before its `gil_only` check
  ([#163](https://github.com/grafana/pyroscope-python/issues/163), with
  [#164](https://github.com/grafana/pyroscope-python/issues/164) and
  [#165](https://github.com/grafana/pyroscope-python/issues/165)). `cpp/stack`
  needs one `#ifdef` for `BITS_TO_PTR_MASKED`, which `pycore_stackref.h` defines
  only in its GIL arm -- but compiling is not the bar, since echion reads struct
  layouts that free-threaded changes.

## Traps

- **`threads::install` must stay after `pyroscope_stack_start`.**
  `Sampler::start` runs `one_time_setup`, which reaches
  `EchionSampler::postfork_child()` and placement-news `thread_info_map_`, so
  any earlier registration is discarded.
- **`stack::stop` only runs when `STARTED`.** `Sampler::stop()` bumps
  `thread_seq_num` unconditionally and `Sampler::prefork` reads its parity, so
  stopping a sampler that never started makes a later fork resurrect it.
- **Never call `stack_init` from `configure`.** Its
  `ThreadSpanLinks::postfork_child` placement-news a mutex the never-removed
  `threading` patch can be holding.
- **`init_safe_copy` stays under its `std::once_flag`.** A second run would
  install our SIGSEGV handler on top of a foreign one and undo
  `sampling_thread`'s permanent fallback.
- **`target_overhead` is a fraction here**, a `1..100` percentage in upstream's
  `stack.py`.
- **Restore the SYSTEM marking on `stack/include/util`** if
  `stack/src/stack.cpp` ever comes back -- it silenced `-Wold-style-cast` and
  `-Wcast-function-type-mismatch` on `cast_to_pyfunc.hpp`.
- **Restore `ThreadSpanLinks::reset()` and `native_call_registry.reset()`** in
  `pyroscope_stack_stop` with whichever feature starts populating them.
- **Do not spell `BITS_TO_PTR_MASKED` as `PyStackRef_AsPyObjectBorrow`.** Under
  `Py_STACKREF_DEBUG` the latter consults a debug table, which is wrong for a
  stackref copied out of another process.
- **On the next vendor sync, re-delete `Datadog::PauseResult`.** It returns in
  `sampler.hpp` and in three `Sampler::pause` returns; keeping
  `SamplerPauseResult` is what makes an upstream variant change a compile error.

## Done

- ~~Fast copy is opt-in: `cpu_fast_copy`, `cpu_fast_copy_warmup`, handlers
  installed from `configure()` and fixed for the process.~~
- ~~Adaptive sampling reaches the sampler: `cpu_adaptive_sampling`,
  `cpu_adaptive_target_overhead`, `cpu_adaptive_max_interval_us`.~~
- ~~`profile.period` comes from `Sampler::get_interval_us()`, read at dump time.~~
- ~~`cpu_max_nframe` and `cpu_max_threads`.~~
- ~~FFI push path: `pyroscope_push_sample` takes a `PprofBuilderType`, and
  `FFISampleValues` carries `cpu_time` / `wall_time`.~~
- ~~`PProfBuilder<K>` is parameterized by a `ProfileKind` (`MemoryProfile`,
  `CpuWallProfile`, `PySpyProfile`), which owns the value layout and `period`.~~
- ~~One profile, not two: `process_cpu` carries both `cpu/nanoseconds` and
  `wall/nanoseconds`.~~
- ~~`crate::stack::dump_pprof`: interner lock before the builder lock, no GIL,
  no `extern "C"` gating.~~
- ~~`upload_seq` advances via `pyroscope_stack_bump_upload_seq`, so
  `clear_ephemeral()` runs.~~
- ~~Fork handling: the child cleans up and never restarts, and the interner and
  builder mutexes are `forksafety::LeakableMutex`es leaked and replaced in the
  child. Covered by `fork_workload.py`.~~
- ~~The Rust agent starts the sampler
  (`cpu_implementation=ProfilerImplementation.Stack`); there is no `_stack`
  module and no `PyMethodDef` table.~~
- ~~`import pyroscope` no longer installs signal handlers: the `stack_init` and
  `init_safe_copy` constructor attributes are dropped.~~
- ~~`faulthandler.enable` / `disable` are patched once per process, and only
  when fast copy is on.~~
- ~~A missing `faulthandler` module no longer fails `configure()`.~~
- ~~`Sampler::pause()` returns the cbindgen `SamplerPauseResult`.~~
- ~~`ffikit::run` no longer holds `STATE` across GIL-needing startup.~~
- ~~`stack/src/stack.cpp` and `stack/include/util/cast_to_pyfunc.hpp` deleted.~~

## Verification

`scripts/tests/test_stack_cpu.py` is the only test that proves samples are
produced. Build commands are in `CLAUDE.md`.

- After touching `ffikit`, run `test_memory.py`, `test_concurrency.py` and
  `test_atexit.py` too, and run the concurrency shape with
  `cpu_implementation=Stack` -- that is what caught the `STATE`/GIL deadlock,
  and the shipped test only exercises `mem_enabled=True`.
- Iterate on Python 3.13, including
  `PYTHON_VERSION=3.13 go test -run '^TestPythonSignalHandlerSuites$' .` from
  `integration-test/`. The 3.10-3.14, musl and amd64 matrix is CI's. `orb` has
  only 3.12 system-wide; use `uv` for 3.13 there.
- macOS alone proves little: `PL_LINUX` selects different code in `vm.cc` and
  `danger.cc`, and `is_safe_copy_failed` is hardcoded `false` on Darwin. Build
  on Linux too (`ssh orb`).
