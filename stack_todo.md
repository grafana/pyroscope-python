# CPU stack profiler: outstanding work

Work list for `profiling/stack/` (dd-trace-py's echion-based CPU sampler) and the
`profiling/dd_wrapper/` shims under it. Defects we have decided to live with, and the
TODOs inherited from the vendored code, are in `stack_known_bugs.md`. Things
this iteration has decided not to do are in `stack_scope.md`. Do not re-file
either kind as work here.

Format: one bullet per item, a sentence or two. A finished item is deleted, not
kept as a record. Cite a symbol, not a line number.

## Open

- **Investigate reusing dd-trace-py's Python source.** Assess whether it can
  replace the reimplementations in `rust/src/stack.rs` (`threads`, `asyncio`)
  and the Python modules defined from strings via `PyModule::from_code`.
- **Measure what `PyModule::from_code` costs at start.** `threads::install`,
  `asyncio::install` and `faulthandler::install` each compile their `INSTALL_SRC`
  from source at startup; compare against shipping them as `.py` files whose
  `.pyc` CPython can cache.

- **Fold the stack module's globals into `ffikit::STATE`.** `STARTED`,
  `OPTIONS`, `threads`/`asyncio`/`faulthandler`'s `INSTALLED` and
  `PROFILE_BUILDER` each carry their own once-per-process lifetime, which is
  the session lifetime `STATE` already tracks; moving them there would make the
  start/stop ordering one piece of state instead of five.

- **Close the `async_tracking` install window.** `asyncio::install` reads
  `sys.modules` once, so a module imported later is never patched; both misses
  warn now, but no profile distinguishes them from an idle loop:
  - Patch `asyncio` and `uvloop` when they are imported, not when the agent
    starts, as upstream's `ModuleWatchdog.after_module_imported` does. The
    hook must also fire for a module already imported by `configure()`.
  - `mod asyncio` is once-per-process, so a later `configure()` with
    `async_tracking=False` still unwinds tasks through the first call's wrappers.
  - `test_stack_async.py`'s `CPU_ASYNC=0` check reads one snapshot after the
    first `async_cpuburn` hit, which can precede any sample of the idle task;
    it needs a settle window to mean anything.
- **Reach the greenlet/gevent unwinder.** `Sampler::track_greenlet`,
  `untrack_greenlet`, `link_greenlets` and `record_greenlet_switch` have no
  caller and no FFI export; upstream's entry point is
  `_task.initialize_gevent_support()`, and it reads thread idents from a
  pre-monkeypatch `threading` that `stack::threads` has no equivalent of.
  gevent ships no cp310 aarch64 wheel, so its workload needs a `ci.yml` exclusion.

## Traps

- **`asyncio::install` must stay after `threads::install`.**
  `track_asyncio_loop` and `set_uvloop_mode` are find-then-mutate on echion's
  thread map, so a loop tracked before its thread is registered is dropped and
  the thread unwinds as a plain stack; only `set_uvloop_mode` warns about it.
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
- **On every vendor sync, check `Datadog::PauseResult` against `stack::PauseResult`.**
  The Rust side is a hand-written `repr(u8)` mirror, so a new or renumbered
  upstream variant compiles silently and reaches `pause_sampling` as an invalid value.
- **`fast_copy=True` is refused for embedded interpreters.** Upstream's
  `is_python_embedded()` in `init_safe_copy` treats an unreadable
  `/proc/self/exe` as embedded, so fast copy silently stays on the syscall copy.

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
