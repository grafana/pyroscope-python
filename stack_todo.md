# CPU stack profiler: outstanding work

Tracking doc for `cpp/stack/` (dd-trace-py's echion-based CPU sampler) and the
`cpp/dd_wrapper/` shims under it. Written when the tree was first made to
compile, so it covers both the gaps that port opened and the TODOs that came
along with the vendored code.

Status: **the sampler is linked into the extension and its threads register,
but nothing starts it.** Everything from the FFI boundary inward is wired -- a
`CpuWall` sample that reaches `pyroscope_push_sample` is accumulated, encoded
and uploaded -- and `cpu_implementation=ProfilerImplementation.Stack` now
populates echion's thread info map. What is left is `Sampler::start()`/`stop()`.

## 1. Blocking: nothing produces CPU samples

The whole point, and everything else in section 2 is downstream of it.

The push path is complete end to end. `Sample::push_cputime`/`push_walltime`
accumulate into `FFISampleValues.cpu_time`/`wall_time`, every `Sample` carries
the `PprofBuilderType` it was constructed with, and `flush_sample()` forwards to
`export_sample()`, which calls
`pyroscope_push_sample(builder_type, frames, len, values)`. Forwarding is safe
because the type tells Rust which profile the sample belongs to -- it is the
memory *projection*, not the value struct, that was ever memory-specific.
`pyroscope_push_sample` dispatches `CpuWall` into `crate::stack`, which
accumulates it and hands a pprof to the agent's upload window alongside the
memory profile.

What is missing now is a *producer*. The extension does reference the sampler
since thread registration landed, so `cpp/stack` is no longer dropped from the
`.so`, but nothing calls `Datadog::Sampler::start()` and the accumulator in
`crate::stack` is always empty. That item is "Nothing starts the sampler" in
section 2 and is the remaining blocker.

- ~~**New FFI surface.**~~ Done. `FFISampleValues` carries `cpu_time` and
  `wall_time` alongside the four memory slots, named after upstream's
  `ValueIndex`. The time slots are signed, the memory ones are not. There are
  no `cpu_count`/`wall_count` slots: every call site passes a count of 1, so
  the sample tally is whatever the encoder counts merging into a pprof row.
- ~~**A push entry point carrying the profile type.**~~ Done.
  `pyroscope_push_sample` replaces `pyroscope_memprof_push_sample` and takes a
  `PprofBuilderType` first argument. It lives in `rust/src/ffi.rs`, ungated, and
  is the only place that handles the raw pointers. The dispatch is an exhaustive
  `match`, so adding a variant without a sink is a compile error rather than a
  silently dropped profile. `Cpu` is py-spy's and is the one arm that stays a
  no-op: py-spy never crosses the FFI boundary.
- ~~**A CPU accumulator in `PProfBuilder`.**~~ Done. `PProfBuilder<K>` is
  parameterized by a `ProfileKind` -- `MemoryProfile`, `CpuWallProfile` or
  `PySpyProfile` -- which names the row's value layout (`K::Values`, one slot
  per sample type), what `period` is derived from (`K::PeriodConfig`), and how
  the sample types are set. The kind replaced the old `builder_type` field and
  the three `assert_eq!`s that guarded it at runtime: a
  `PProfBuilder<MemoryProfile>` can no longer be handed cpu sample types
  because the setter is `K::set_profile_type`.

  `add_ffi_sample` takes the raw `&FFISampleValues` and projects it through
  `K::value_slots`, so a call site cannot pair the wrong projection with a
  builder either. It lives on `impl<K: FfiProfileKind>`, which `PySpyProfile`
  does not implement, so it is not callable on the py-spy builder at all;
  `add_stacktrace` is likewise confined to `impl PProfBuilder<PySpyProfile>`.

  All three `set_profile_type` impls **assign** rather than append, which is
  what lets a dump path re-set the types every window after
  `take_profile_and_reset` has `mem::take`n the profile;
  `cpu_wall_profile_type_is_idempotent` guards that.
  `cpu_wall_projection_reads_only_the_time_slots` guards the projection against
  slot drift and against a memory slot leaking in.
- ~~**One profile, not two.**~~ Done (reversing an earlier call): the sampler
  emits a single pprof carrying both `cpu/nanoseconds` and `wall/nanoseconds`
  sample types, as upstream dd_wrapper does -- one `ReportBatch`, one
  `RawProfileSeries`, one `__name__`. This is why `PprofBuilderType` has a
  single `CpuWall` variant and one `FFISampleValues` carries both times.

  Splitting was rejected on a wrong premise, that Pyroscope keys expected
  sample types off the profile name. It does not: a profile type ID is
  `__name__:sample_type:sample_unit:period_type:period_unit`, so one series
  yields one queryable ID per sample type -- which is exactly how `memory`
  serves four. See the `*ProfileTypeID` constants in
  `integration-test/integration_test.go`.

  The `__name__` is **`process_cpu`**, the same name py-spy publishes
  (`rust/src/pyspy_backend.rs`), which makes the two CPU sources **mutually
  exclusive**. `PyroscopeAgent::snapshot` enforces that: when the py-spy
  backend is running it drops the stack sampler's profile with a warning
  naming `cpu_enabled=False` as the way out. The dump itself still runs, so
  the accumulator drains every window rather than growing without bound. The
  win is that `process_cpu:cpu:nanoseconds:cpu:nanoseconds` stays the type ID
  the UI and the integration tests already know; the sampler adds
  `process_cpu:wall:nanoseconds:cpu:nanoseconds` to it.

  Still open: there is no `samples/count` sample type, since `FFISampleValues`
  has no count slots; a tally would have to come from how many stacks merge
  into a row. And `period` is derived from the agent-wide `sample_rate`, not
  from the sampler's own (adaptive) interval -- that has to be plumbed through
  once something starts the sampler.
- ~~**A dump path.**~~ Done. `crate::stack::dump_pprof` copies the shape of
  `memory::implementation::dump_pprof` and its lock order --
  `interner::string_table()` **before** the profile builder lock, never the
  reverse (the invariant is spelled out on `interner::clear`). It needs neither
  the GIL nor a profiler-side flush, so it drops `Python::try_attach` and has no
  `extern "C"` dependency; the whole module is therefore **ungated**, unlike
  `memory::implementation`. See section 3.
- **Labels have nowhere to go.** `push_threadinfo`, `push_task_name`,
  `push_span_id`, `push_local_root_span_id`, `push_trace_type` and
  `push_monotonic_ns` are still no-ops, and not just for want of struct fields:
  `PProfBuilder`'s FFI accumulator is keyed on the location-id vector alone and
  `flush_ffi_samples` hardcodes `label: vec![]`, so two samples differing
  only by thread or task name are indistinguishable once merged. Carrying them
  means either folding the label set into the accumulator key or giving up
  accumulation for these samples. The py-spy path (`add_stacktrace`) does emit
  labels and pushes each sample directly -- that is the shape to copy.

## 2. Gaps this port opened

### ~~`upload_seq` never advances~~
`cpp/pyroscope/stack_ffi.cpp`, `rust/src/stack.rs`

Done. `crate::stack::dump_pprof` bumps it via
`pyroscope_stack_bump_upload_seq`, standing in for the uploader we do not
vendor. Only a window that yields a profile bumps; one the agent then discards
because py-spy owns `process_cpu` still counts, since the samples were produced
either way.

`clear_ephemeral()` can therefore run now, and it is safe to let it: only
`StringTag::TaskName` is ephemeral (`is_ephemeral` in `echion/strings.h` --
`GreenletName` is deliberately excluded), its one producer is `echion/tasks.cc`,
and its only consumer is the `line == 0` branch of
`StackRenderer::render_frame`, which re-looks-up the key every cycle with a
`missing_name` fallback and never caches it in `string_id_cache`. Worst case
after a clear is one cycle of a task rendering as the fallback name.

This entry used to record a link-scope invariant that **no longer holds**: the
shim once pulled in only `profiler_state.cpp` and `native_call_tracker.cpp`, so
the built dylib had no `__mod_init_func` section at all. Thread registration
changed that -- see "`import pyroscope` now installs signal handlers" below.

### Fork handling is incomplete
`cpp/dd_wrapper/include/profiler_state.hpp`, `rust/src/ffikit.rs`

`ProfilerState::postfork_child()` exists but has no caller, so
`NativeCallRegistry`'s `std::shared_mutex` is never re-initialized in a forked
child. Upstream gets this for free: `ProfilerState::start` installs a
`pthread_atfork` child handler *before* `Sampler::start` installs its own, and
POSIX's FIFO child-handler ordering does the rest. The note in
`Sampler::atfork_child` (`cpp/stack/src/sampler.cpp`) still describes that
arrangement; we have no `ProfilerState::start`, so it does not hold.

This is the same hazard as the existing `TODO(Pyroscope)` on
`ffikit::stop_profilers` (`rust/src/ffikit.rs`), and the two should be fixed
together: `stack_atfork_child` runs inside `os.fork()`, i.e. before Python's
`at_fork_after_in_child` reaches `stop_profilers`, and it calls
`restart_after_fork()` -- so the sampling thread is live again and re-warming
`StackRenderer::string_id_cache` by the time we clear the string table
underneath it. The sampler must be stopped and kept stopped in the child; do
not rely on `renderer_.postfork_child()`.

A third strand now: the child inherits the parent's thread info map, and the
`threading` wrappers are inherited patched, so the child's own `MainThread` is
never re-registered. `Sampler::postfork_child()` rebuilds both the map and that
one entry, but still has no caller. Fix it with the other two.

### Nothing starts the sampler
`cpp/pyroscope/stack_ffi.cpp`, `rust/src/stack.rs`

**This is now the blocking item** -- see section 1.

The driving model is decided: **the Rust agent drives the sampler, there is no
`_stack` Python module and no `PyMethodDef` table.** `configure()` selects the
implementation with `cpu_implementation=ProfilerImplementation.Stack`, which
`initialize_agent` turns into `crate::stack::Config`, and `extern "C"` shims in
`cpp/pyroscope/stack_ffi.cpp` are how Rust reaches `Datadog::Sampler`.
Registration went first because `for_each_thread` skips every thread absent
from the map, so a started sampler with an empty map emits nothing.

What remains is `pyroscope_stack_start` / `pyroscope_stack_stop` shims wrapping
`Sampler::set_*` + `start()` / `stop()`, called from `ffikit::run` and
`ffikit::stop_profilers`, plus `is_safe_copy_failed()` checked before start the
way `StackCollector._init()` does. `cpp/stack/src/stack.cpp` still holds
`PyInit__stack` and its ~30-entry method table; both are dead and should go, and
the handful of entries we still want (`start_native_monitoring` and friends) can
become `extern "C"` at the same time.

Upstream's config order, from `ddtrace/profiling/collector/stack.py::_init()`:
setters, `is_safe_copy_failed()`, `start()`, span hook, native monitoring, then
thread registration.

### Thread registration must stay after `Sampler::start()`
`rust/src/ffikit.rs`

`stack::install_thread_hooks` currently runs *before* anything starts the
sampler, which is only safe because nothing starts it. `Sampler::start()` runs
`std::call_once(one_time_setup)`; `one_time_setup()` reaches
`EchionSampler::postfork_child()`, which does
`new (&thread_info_map_) std::unordered_map<...>()`
(`cpp/stack/echion/echion/echion_sampler.h:122`) -- so every registration made
before `start()` is silently discarded. Upstream orders `_init()` the same way
and says so in a comment. The `TODO(Pyroscope)` at the call site marks it.

### `import pyroscope` now installs signal handlers
`cpp/stack/src/echion/vm.cc`, `cpp/stack/src/sampler.cpp`

Referencing `Sampler::register_thread` pulls `sampler.cpp.o` into the link, and
that object carries `__attribute__((constructor)) stack_init()`
(`cpp/stack/src/sampler.cpp:621`). It calls `_set_pid`, which lives in `vm.cc`
(`cpp/stack/src/echion/vm.cc:174`), so `vm.cc.o` comes too and its own
`init_safe_copy` constructor (`vm.cc:35` on Linux, `:70` on Darwin) runs
`init_segv_catcher()` at load time. So `import pyroscope` chains SIGSEGV/SIGBUS
handlers for **every** user now, including memory-only ones and ones who never
call `configure()`. Confirmed with `otool -l`: the dylib has a
`__mod_init_func` section, which it did not before.

Deliberately accepted for the registration slice. The fix belongs with the
start/stop work: drop both constructor attributes as a `// Pyroscope patch:` and
call the two initializers from an explicit init on the start path, so the
handlers appear only when the sampler does. `_DD_PROFILING_STACK_FAST_COPY=0`
is the only opt-out until then, and it only skips the handler install, not
`stack_init`.

### Nothing unpatches `threading`
`rust/src/stack.rs`

`install_thread_hooks` is once-per-process and there is no uninstall, matching
upstream, which also never unpatches. After `shutdown()` the wrappers keep
calling `register_thread`/`unregister_thread`, so the map stays live with no
agent running. Bounded by the live thread count, since unregistration still
fires on thread exit, but it means `pyroscope_stack_thread_count()` is not zero
between sessions -- do not treat a non-empty map as proof the agent is up.

Also inherited from upstream: threads not created through `threading.Thread`
(raw `_thread.start_new_thread`, C-created threads that attach later,
`_DummyThread`s appearing after the install-time sweep of `threading._active`)
never register, so they are invisible to the sampler. Filling the map from the
tstate snapshot inside `for_each_thread` would cover them, at the cost of
calling `pthread_getcpuclockid` / `pthread_mach_thread_np` on a `pthread_t` read
out-of-band -- a use-after-free if that thread has since exited.

### Wall time is multiplied by the task count

`cpp/stack/src/echion/threads.cc`, `cpp/stack/src/stack_renderer.cpp`

`ThreadInfo::sample` renders one sample per leaf asyncio task (or per greenlet
stack), and *every* one of them pushes `push_walltime(thread_state.wall_time_ns,
1)` with the same per-cycle thread delta. A thread with 50 tasks contributes 50x
the elapsed wall time for that cycle.

This is upstream's intended per-task attribution, not a bug, but it means the
wall total is **not conservative** and must never be sanity-checked against
wall-clock elapsed. Worth stating explicitly wherever a `wall` profile is
eventually documented, because the numbers look wrong otherwise.

### The one-CPU-sample-per-thread invariant rests on two swap loops

`cpp/stack/src/echion/threads.cc`

`push_cputime` is unconditional on a thread's first sample (via
`render_cpu_time`) and conditional on `on_cpu` for every sample after it. So
avoiding double-counted CPU time depends entirely on the on-CPU task/greenlet
being hoisted to index 0, where `render_task_begin` reuses the already-credited
sample and never reaches its `if (on_cpu)` branch.

`threads.cc` does that hoist in two places -- once in `unwind_tasks` for
asyncio, once for greenlets -- with slightly different loop bounds (`i = 0` plus
an `if (i > 0)` guard versus `i = 1`). The greenlet one carries an explicit
warning that it silently no-ops, restoring the over-count, if greenlet's
`Py_None` "currently running" sentinel ever changes. Neither is covered by a
test, and the failure is a plausible-looking 2x rather than a crash.

### `max_nframes` is not configurable
`cpp/dd_wrapper/include/sample_manager.hpp`

Hardcoded to `g_default_max_nframes` (64). Upstream plumbs
`SampleManager::set_max_nframes` from Python config, clamping to
`g_backend_max_nframes` (512); that path lives in the Cython layer we do not
vendor. Note `configure()` already takes `mem_max_nframe` for the memory
profiler -- a CPU equivalent should follow it.

### `SampleManager::start_sample` relies on unenforced invariants
`cpp/dd_wrapper/include/sample_manager.hpp`

The `thread_local` single-instance replacement for upstream's
`StaticSamplePool` is sound only because (a) only the sampling thread renders
and (b) `render_stack_end` always pairs `flush_sample()` with `drop_sample()`
before the next `start_sample()`. Both hold today; neither is asserted. If a
second renderer thread ever appears, revisit before debugging the corruption.

### `ProfilerStats` and `ProfileBorrow` are inert
`cpp/pyroscope/Pyroscope.h`

All 11 stat setters drop their argument. Datadog ships these alongside the
profile in an internal-metadata channel Pyroscope has no equivalent for. Worth
revisiting for our own diagnostics: `set_string_table_count`,
`add_sample_capture_cpu_time_us` and `add_copy_memory_error_count` in
particular are real health signals the sampler is already computing and we are
already discarding.

Related: `ProfileBorrow` is stateless, so it holds no lock. Upstream's whole
reason for the `auto borrow = ...` batching in `Sampler::sampling_thread` is to
keep one upload window's counters together. If stats ever become real, that
batching has to become real too.

### Frame addresses are dropped
`cpp/pyroscope/Pyroscope.h` (existing `TODO(Pyroscope)` on `push_frame`)

`FFIFrame` has no address field and `add_location_mirror` hardcodes
`ddog_prof_Location.address` and `mapping_id` to 0. Nothing of value is lost
today; the only caller that passed a nonzero value used a literal `1` as an
undocumented sentinel for native frames, which stay distinguishable by name and
filename.

### `NativeCallRegistry::size()` is reconstructed, not vendored
`cpp/dd_wrapper/include/native_call_tracker.hpp`

`cpp/stack` was copied from a dd-trace-py revision newer than the `dd_wrapper`
sources available locally, and `stack.cpp`'s `_native_call_registry_size` test
helper calls a `size()` that does not exist upstream yet. Ours is a
`shared_lock` + `call_sites.size()`. Replace it with upstream's on the next
vendor sync if the signature differs.

### `-Werror` is off
`cpp/CMakeLists.txt`

`cpp/stack` now compiles warning-free under Apple clang and gcc 13.3 with
`-Wall -Wextra -Wshadow -Wnon-virtual-dtor -Wold-style-cast`. It is still off
because release wheels build in manylinux/musllinux images on older gcc whose
diagnostics we have not seen, and a warning we cannot reproduce locally would
hard-fail a release rather than the dev loop. Turn it on once CI has been green
across the full image matrix for a while.

Unrelated blocker for turning it on more broadly: `cpp/memalloc/_memalloc.cpp`
compares an unsigned `heap_sample_size < 0` (gcc `-Wtype-limits`). Pre-existing,
and memalloc does not get this warning set upstream either.

## 3. Free-threaded builds

`rust/build.rs` early-returns under `cfg!(not(feature = "memory"))`, and
`setup.py` only sets that feature when `Py_GIL_DISABLED != 1` -- so on
free-threaded builds no C++ is compiled at all, CPU sampler included. The whole
Rust half of the CPU path is ungated in anticipation of exactly this: the
interner (`rust/src/encode/interner.rs`), the push path (`rust/src/ffi.rs`), and
the accumulator and dump path (`rust/src/stack.rs`). Only `crate::memory`'s
accumulator and dump path are behind the gate, because they call `memalloc_*`.
What is left is the C++ side: decide whether the CPU sampler ships on
free-threaded builds and, if so, split the feature gate so the stack half builds
without `memory`.

## 4. TODOs inherited from the vendored code

Upstream's, not ours. Listed so they are not mistaken for port artifacts.

- `cpp/stack/src/stack_renderer.cpp:221` -- thread-level CPU time is attributed
  to task time, which upstream calls "absolutely false". Needs normalizing to
  the task level; kept because that is how the v1 sampler behaved.
- `cpp/stack/src/stack_renderer.cpp:147` -- echion pushes a dummy frame holding
  the task name (line number 0). The renderer consumes it as the task name and
  returns early rather than also emitting it as a frame. Upstream wonders
  whether emitting it would be clearer.
- `cpp/stack/src/echion/threads.cc:266` -- the on-CPU task's stack may be
  mismatched against the thread stack if the task that was on CPU at capture
  time differs from the one we think is. Fixing it means matching every task
  stack against the thread stack; upstream reports never having observed the
  race.
- `cpp/stack/src/echion/tasks.cc:206` -- `TaskInfo::unwind` does not check for a
  running task.
- `cpp/stack/src/echion/stacks.cc:95` -- on Python 3.11/3.12, a failed
  `_PyCFrame` copy returns silently instead of signalling an invalid frame.

## Verification notes

The linker no longer drops the whole stack half -- registration references it --
but it still keeps only what is reachable, so most of `cpp/stack` remains absent
from the `.so`. Check the archive while iterating:

```
cmake -S cpp -B /tmp/ddbuild -G Ninja \
  -DPython3_EXECUTABLE=$(which python3) -DPython3_FIND_STRATEGY=LOCATION
cmake --build /tmp/ddbuild

# the stack objects really got archived
ar t /tmp/ddbuild/libdatadog_mem_profiler.a | grep -E 'sampler|stack|profiler_state'

# no unresolved project symbols (only the two Rust FFI exports should remain)
nm -u /tmp/ddbuild/libdatadog_mem_profiler.a | c++filt | grep Datadog::

# what actually made it into the extension (C++ symbols are hidden, so no -g)
nm -a build/lib.*/pyroscope/_native*.so | c++filt | grep Datadog::Sampler
```

`cpp/stack` is heavily `PY_VERSION_HEX`-gated, so repeat for 3.11 through 3.14.
`PL_LINUX` selects different code in `vm.cc` (`process_vm_readv`) and
`danger.cc`, so a macOS-only build proves little -- use `ssh orb`, where the mac
sources are mounted at identical paths.
