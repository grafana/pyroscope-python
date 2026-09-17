# CPU stack profiler: outstanding work

Tracking doc for `cpp/stack/` (dd-trace-py's echion-based CPU sampler) and the
`cpp/dd_wrapper/` shims under it. Written when the tree was first made to
compile, so it covers both the gaps that port opened and the TODOs that came
along with the vendored code.

Status: **the static library compiles and archives; the sampler produces no
data.** It walks stacks correctly and throws every sample away. Nothing in
Python imports it yet.

## 1. Blocking: CPU samples reach Rust and are dropped there

The whole point, and everything else in section 2 is downstream of it.

The C++ side is wired up end to end. `Sample::push_cputime`/`push_walltime`
accumulate into `FFISampleValues.cpu_time`/`wall_time`, every `Sample` carries
the `PprofBuilderType` it was constructed with, and `flush_sample()` forwards to
`export_sample()`, which calls
`pyroscope_push_sample(builder_type, frames, len, values)`. Forwarding is safe
because the type tells Rust which profile the sample belongs to -- it is the
memory *projection*, not the value struct, that was ever memory-specific.

`pyroscope_push_sample` then early-returns for anything but
`PprofBuilderType::Memory`, so a CPU sample dies one frame later than it used
to. (In practice `flush_sample` never runs yet either, because nothing imports
the extension -- see "Nothing imports the extension" below.)

Still to do, in order:

- ~~**New FFI surface.**~~ Done. `FFISampleValues` carries `cpu_time` and
  `wall_time` alongside the four memory slots, named after upstream's
  `ValueIndex`. The time slots are signed, the memory ones are not. There are
  no `cpu_count`/`wall_count` slots: every call site passes a count of 1, so
  the sample tally is whatever the encoder counts merging into a pprof row.
- ~~**A push entry point carrying the profile type.**~~ Done.
  `pyroscope_push_sample` replaces `pyroscope_memprof_push_sample` and takes a
  `PprofBuilderType` first argument. One caveat: it still lives in
  `rust/src/memory.rs` behind the `memory` feature, and a cpu/wall sink must
  not -- see section 3. Moving it out is the next step.
- **A CPU accumulator in `PProfBuilder`.** `add_ffi_sample` takes already
  projected `[i64; 4]` slots, so it can be reused rather than duplicated; what
  a cpu/wall profile needs is its own projection alongside
  `memory_value_slots`, and `memory_samples` made const-generic if the slot
  count differs from 4.
  `set_cpu_profile_type` already exists and sets `cpu/nanoseconds` + period.
  Two hazards there: it **pushes** a sample type while
  `set_memory_profile_type` **assigns**, so they cannot be composed as-is; and
  `take_profile_and_reset` does `mem::take` on the profile, wiping
  `sample_type`/`period`/`period_type`, which is why `dump_pprof` re-sets the
  type every window.
  `memory_projection_reads_only_the_memory_slots` guards the existing
  projection against slot drift and against a time slot leaking in.
- **One profile, not two.** Decided (reversing an earlier call): the sampler
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

  Still open: the `__name__` value. py-spy already emits `process_cpu`
  (`rust/src/pyspy_backend.rs`), so either the two CPU sources are made
  mutually exclusive or this one needs a different name. Also note there is no
  `samples/count` sample type available, since `FFISampleValues` has no count
  slots; a tally would have to come from how many stacks merge into a row.
- **A dump path.** Copy the shape of `memory::implementation::dump_pprof` and
  keep its lock order: `interner::string_table()` **before** the profile
  builder lock, never the reverse. The invariant is spelled out on
  `interner::clear` in `rust/src/encode/interner.rs`.
- **Labels have nowhere to go.** `push_threadinfo`, `push_task_name`,
  `push_span_id`, `push_local_root_span_id`, `push_trace_type` and
  `push_monotonic_ns` are still no-ops, and not just for want of struct fields:
  `PProfBuilder`'s FFI accumulator is keyed on the location-id vector alone and
  `flush_memory_samples` hardcodes `label: vec![]`, so two samples differing
  only by thread or task name are indistinguishable once merged. Carrying them
  means either folding the label set into the accumulator key or giving up
  accumulation for these samples. The py-spy path (`add_stacktrace`) does emit
  labels and pushes each sample directly -- that is the shape to copy.

## 2. Gaps this port opened

### `upload_seq` never advances
`cpp/dd_wrapper/include/profiler_state.hpp`

Upstream bumps it once per upload in the uploader we do not vendor.
`Sampler::sampling_thread` watches the delta and calls
`echion->string_table().clear_ephemeral()` every 25 uploads
(`ephemeral_clear_interval` in `cpp/stack/src/sampler.cpp`). Held at 0, that
clear never runs, so echion's ephemeral table -- asyncio task names, mainly --
grows without bound in a process that churns task names. Bump it from whatever
dump path section 1 produces.

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

### Nothing imports the extension
`cpp/stack/src/stack.cpp`

`PyInit__stack` and the ~30-entry `stack_methods` table are compiled but no
Python module loads `_stack`, and no Rust symbol references any `stack.cpp`
object -- so the linker drops the entire stack half from the final cdylib.
Verified: `nm` on `libpyroscope_python_extension.dylib` finds zero
`Datadog::Sampler` / `StackRenderer` / `EchionSampler` symbols. A compile check
must therefore inspect the static archive, not the `.so`.

Decide whether the Python side drives the sampler through `_stack`'s
`PyMethodDef` table (upstream's model) or through the Rust agent, and wire
`cpu_enabled` in `python/pyroscope/__init__.py::configure` to it.

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
free-threaded builds no C++ is compiled at all, CPU sampler included. The
interner was already moved out of `crate::memory` into
`rust/src/encode/interner.rs` in anticipation of exactly this. Decide whether
the CPU sampler ships on free-threaded builds and, if so, split the feature gate
so the stack half builds without `memory`.

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

Useful while iterating, since the stack half is invisible in the final `.so`:

```
cmake -S cpp -B /tmp/ddbuild -G Ninja \
  -DPython3_EXECUTABLE=$(which python3) -DPython3_FIND_STRATEGY=LOCATION
cmake --build /tmp/ddbuild

# the stack objects really got archived
ar t /tmp/ddbuild/libdatadog_mem_profiler.a | grep -E 'sampler|stack|profiler_state'

# no unresolved project symbols (only the two Rust FFI exports should remain)
nm -u /tmp/ddbuild/libdatadog_mem_profiler.a | c++filt | grep Datadog::
```

`cpp/stack` is heavily `PY_VERSION_HEX`-gated, so repeat for 3.11 through 3.14.
`PL_LINUX` selects different code in `vm.cc` (`process_vm_readv`) and
`danger.cc`, so a macOS-only build proves little -- use `ssh orb`, where the mac
sources are mounted at identical paths.
