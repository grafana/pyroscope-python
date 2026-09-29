# CPU stack profiler: outstanding work

Work list for `cpp/stack/` (dd-trace-py's echion-based CPU sampler) and the
`cpp/dd_wrapper/` shims under it. Defects we have decided to live with, and the
TODOs inherited from the vendored code, are in `stack_known_bugs.md`. Things
this iteration has decided not to do are in `stack_scope.md`. Do not re-file
either kind as work here.

Format: one bullet per item, a sentence or two. A finished item is deleted, not
kept as a record. Cite a symbol, not a line number.

## Open

- **The new upstream knobs are not exposed.** `set_gc_enabled`,
  `set_max_tasks_per_sample`, `set_baseline_core_pct`, `set_p_stable_window_s`
  and `set_p_stable_percentile` all sit at their upstream defaults because
  `pyroscope_stack_configure` does not pass them; `configure()` has no kwarg
  for any of them.
- **The sampling thread can die unreported.** `Sampler::sampling_thread`
  catches, stashes and `break`s; `take_sampling_thread_error` has no caller
  here, so a dead sampler looks like an idle one.
- **Report the sampler's own counters.** `ProfilerStats` drops every setter,
  fast-copy flags included, while `Sampler::sampling_thread` computes all of
  them each cycle; `copy_memory_error_count` and `sample_capture_cpu_time_us`
  are the two worth a sink.
- **Non-UTF-8 frame names are undefined behaviour.**
  `pyroscope_string_table_intern_string` calls `from_utf8_unchecked` on bytes
  copied out of another process. libdatadog sanitized lossily at that boundary;
  our interner replaced it without replacing the check.
- **Reach the asyncio task unwinder.** `Sampler::init_asyncio` and
  `track_asyncio_loop` compile with no caller and no FFI export; upstream drives
  them from `ddtrace/profiling/_asyncio.py`, which has no equivalent here.
- **Reach the greenlet/gevent unwinder.** `Sampler::track_greenlet`,
  `untrack_greenlet` and `link_greenlets` are unreachable for the same reason;
  upstream's entry point is `_task.initialize_gevent_support()`.
- **Reach uvloop unwinding.** `Sampler::set_uvloop_mode` has no caller; upstream
  sets it from `_asyncio.py` once it detects a uvloop event loop.

## Traps

- **`threads::install` must stay after `pyroscope_stack_start`.**
  `Sampler::start` runs `one_time_setup`, which reaches
  `EchionSampler::postfork_child()` and placement-news `thread_info_map_`, so
  any earlier registration is discarded.
- **Never call `stack_init` from `configure`.** Its `SpanLinks::postfork_child`
  and `OriginTaskLinks::postfork_child` placement-new mutexes the never-removed
  `threading` patch can be holding.
- **`init_safe_copy` stays under its `std::once_flag`.** A second run would
  install our SIGSEGV handler on top of a foreign one and undo
  `sampling_thread`'s permanent fallback.
- **`target_overhead` is a fraction here**, a `1..100` percentage in upstream's
  `stack.py`.
- **Restore the SYSTEM marking on `stack/include/util`** if
  `stack/src/stack.cpp` ever comes back -- it silenced `-Wold-style-cast` and
  `-Wcast-function-type-mismatch` on `cast_to_pyfunc.hpp`.
- **Restore `SpanLinks::reset()`, `OriginTaskLinks::disable_and_reset()` and
  `native_call_registry.reset()`** in `pyroscope_stack_stop` with whichever
  feature starts populating them.
- **`reset_string_cache` only runs when `STARTED`.** It rides on
  `pyroscope_stack_stop`, while `interner::clear` runs unconditionally, so the
  two agree only because the cache cannot be non-empty unless the sampler
  sampled. Anything that interns outside a started sampler needs the reset moved
  onto the teardown path itself.
- **Do not spell `BITS_TO_PTR_MASKED` as `PyStackRef_AsPyObjectBorrow`.** Under
  `Py_STACKREF_DEBUG` the latter consults a debug table, which is wrong for a
  stackref copied out of another process.
- **On every vendor sync, re-delete `Datadog::PauseResult`.** It comes back in
  `sampler.hpp` and in three `Sampler::pause` returns; keeping
  `SamplerPauseResult` is what makes an upstream variant change a compile error.
- **`cpu_fast_copy=True` is refused for embedded interpreters.** Upstream's
  `is_python_embedded()` in `init_safe_copy` treats an unreadable
  `/proc/self/exe` as embedded, so fast copy silently stays on the syscall copy.
- **Enabling GC frames takes more than `set_gc_enabled`.** Upstream's
  `stack_start_impl` also pairs `GCFrameTracker::install_current_interpreter()`
  with an uninstall on stop, both under the GIL.

## Verification

`scripts/tests/test_stack_cpu.py` is the only test that proves samples are
produced, and `scripts/tests/test_truncated_frames.py` the only one that proves
a truncated stack carries its `<truncated>` marker. Build commands are in
`CLAUDE.md`.

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
