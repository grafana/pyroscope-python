# CPU stack profiler: outstanding work

Work list for `cpp/stack/` (dd-trace-py's echion-based CPU sampler) and the
`cpp/dd_wrapper/` shims under it. Defects we have decided to live with, and the
TODOs inherited from the vendored code, are in `stack_known_bugs.md`. Things
this iteration has decided not to do are in `stack_scope.md`. Do not re-file
either kind as work here.

Format: one bullet per item, a sentence or two. A finished item is deleted, not
kept as a record. Cite a symbol, not a line number.

## Open

- **No samples after `configure()` -> `shutdown()` -> `configure()`.**
  `StackRenderer::string_id_cache` outlives the `interner::clear()` that
  `ffikit::stop_profilers` runs on shutdown, so every second-run upload is
  rejected with `400 function name string index out of range`. Pinned red by
  `TestPythonStackProfilerRestart`.
- **Fix the fast-copy warmup handler-swap gap**, and file it upstream and here.
  Gate `uninstall_segv_handler` / `reinstall_segv_handler` on "fast copy
  requested and `safe_memcpy_initialized`" rather than on `fast_copy_active`;
  `enable_during_warmup_falls_back` pins today's behaviour, so the fix flips a
  test. See `stack_known_bugs.md`.

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
