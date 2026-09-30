# CPU stack profiler: outstanding work

Work list for `cpp/stack/` (dd-trace-py's echion-based CPU sampler) and the
`cpp/dd_wrapper/` shims under it. Defects we have decided to live with, and the
TODOs inherited from the vendored code, are in `stack_known_bugs.md`. Things
this iteration has decided not to do are in `stack_scope.md`. Do not re-file
either kind as work here.

Format: one bullet per item, a sentence or two. A finished item is deleted, not
kept as a record. Cite a symbol, not a line number.

## Open

- **Investigate reusing dd-trace-py's Python source.** Assess whether it can
  replace the reimplementations in `rust/src/stack.rs` (`threads`, `asyncio`)
  and the Python modules defined from strings via `PyModule::from_code`.
- **Close the `async_tracking` install window, and make it loud until then.**
  `asyncio::install` reads `sys.modules` once, so task unwinding can silently
  do nothing and no profile distinguishes that from an idle loop:
  - Patch `asyncio` and `uvloop` when they are imported, not when the agent
    starts, as upstream's `ModuleWatchdog.after_module_imported` does. The
    hook must also fire for a module already imported by `configure()`.
  - Until that lands, warn when `uvloop` is absent from `sys.modules`, the way
    the `asyncio` branch already does. `uvloop` imported after `configure()`
    loses `track_asyncio_loop` entirely, so `unwind_tasks` never runs and
    nothing is logged.
  - `mod asyncio` is once-per-process, so a later `configure()` with
    `async_tracking=False` still unwinds tasks through the first call's wrappers.
  - `test_stack_async.py`'s `CPU_ASYNC=0` check reads one snapshot after the
    first `async_cpuburn` hit, which can precede any sample of the idle task;
    it needs a settle window to mean anything.
- **The new upstream knobs are not exposed.** `set_gc_enabled`,
  `set_max_tasks_per_sample`, `set_baseline_core_pct`, `set_p_stable_window_s`
  and `set_p_stable_percentile` all sit at their upstream defaults because
  `pyroscope_stack_configure` does not pass them; `configure_cpu_profiler` has
  no kwarg for any of them.
- **The sampling thread can die unreported.** `Sampler::sampling_thread`
  catches, stashes and `break`s; `take_sampling_thread_error` has no caller
  here, so a dead sampler looks like an idle one.
- **Report the sampler's own counters.** `ProfilerStats` drops every setter,
  fast-copy flags included, while `Sampler::sampling_thread` computes all of
  them each cycle; `copy_memory_error_count` and `sample_capture_cpu_time_us`
  are the two worth a sink.
- **Reach the greenlet/gevent unwinder.** `Sampler::track_greenlet`,
  `untrack_greenlet`, `link_greenlets` and `record_greenlet_switch` have no
  caller and no FFI export; upstream's entry point is
  `_task.initialize_gevent_support()`, and it reads thread idents from a
  pre-monkeypatch `threading` that `stack::threads` has no equivalent of.

## Traps

- **`asyncio::install` must stay after `threads::install`.**
  `track_asyncio_loop` and `set_uvloop_mode` are find-then-mutate on echion's
  thread map, so a loop tracked before its thread is registered is dropped with
  no diagnostic, and the thread then unwinds as a plain stack.
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
  stackref copied out of another thread without the GIL.
- **On every vendor sync, re-delete `Datadog::PauseResult`.** It comes back in
  `sampler.hpp` and in three `Sampler::pause` returns; keeping
  `SamplerPauseResult` is what makes an upstream variant change a compile error.
- **`fast_copy=True` is refused for embedded interpreters.** Upstream's
  `is_python_embedded()` in `init_safe_copy` treats an unreadable
  `/proc/self/exe` as embedded, so fast copy silently stays on the syscall copy.
- **Enabling GC frames takes more than `set_gc_enabled`.** Upstream's
  `stack_start_impl` also pairs `GCFrameTracker::install_current_interpreter()`
  with an uninstall on stop, both under the GIL.

## Verification

`scripts/tests/test_stack_cpu.py` is the only test that proves samples are
produced; `test_stack_async.py` does the same for the task unwinder, and its
`CPU_ASYNC`, `UVLOOP` and `CONFIGURE_INSIDE_LOOP` env knobs select the four
shapes worth running; `test_truncated_frames.py` proves a truncated stack
carries its `<truncated>` marker. Build commands are in `AGENTS.md`.

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
