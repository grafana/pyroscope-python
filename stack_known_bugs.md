# CPU stack profiler: known bugs we are not fixing

Accepted defects in `cpp/stack/`, `cpp/dd_wrapper/`, `cpp/pyroscope/stack_ffi.cpp`
and `rust/src/stack.rs`. Work lives in `stack_todo.md`; an upstream bug never
moves there -- we document it here and leave it. Things this iteration has
decided not to do are in `stack_scope.md`. Most of these stay because a local
fix to vendored logic is a vendor-sync conflict we carry forever.

Format: heading plus one to three lines -- where it is, what goes wrong, why it
stays. Cite a symbol, not a line number.

## Verbatim from upstream

### A failing `faulthandler.enable()` leaves faulthandler disabled

`rust/src/stack.rs` (`_patched_enable`). It runs `_original_disable()` before
`_original_enable(*args)`, so `enable(file=closed_file)` raises and drops crash
reporting that was active before the call. Not restorable faithfully: there is
no getter for faulthandler's `file` or `all_threads`.

### `faulthandler.enable()` clobbers foreign handlers on its other signals

`rust/src/stack.rs` (`_patched_enable`). The `disable()` + `enable()` pair stops
faulthandler recording itself as its own previous handler, but `disable()`
restores the saved handler for all five of its signals, so a handler installed
between two `enable()` calls is wiped. Reachable only with fast copy on, which
is not the default.

### `enable()` during the fast-copy warmup loses fast copy permanently

`cpp/pyroscope/stack_ffi.cpp` (`pyroscope_stack_uninstall_segv_handler`).
Both swaps act only `if (fast_copy_active)`, which `sampling_thread` holds false
for the whole warmup, so faulthandler lands on top of our handler and the
process stays on the syscall copy. Pinned by `enable_during_warmup_falls_back`.

### Thread CPU time is reported as task CPU time

`cpp/stack/src/stack_renderer.cpp` (`render_cpu_time`), carrying upstream's own
"absolutely false" comment. Normalizing to the task level is upstream's open
work.

### Wall time is multiplied by the leaf task count

`cpp/stack/src/echion/threads.cc` (`ThreadInfo::sample`). Every leaf asyncio
task or greenlet stack pushes the same per-cycle thread delta, so 50 tasks
contribute 50x the elapsed wall time. Upstream's intended per-task attribution;
the consequence is that `wall` totals must never be checked against wall-clock
elapsed.

### The task credited as on-CPU may not be the one that was on CPU

`cpp/stack/src/echion/threads.cc` (`unwind_tasks`, upstream's TODO). The thread
stack is captured out-of-band, so the coroutine and sync halves can be
mis-spliced. Upstream's fix -- match every task stack against the thread stack
-- costs real work per sample, and they report never observing the race.

### The task-name frame is consumed rather than also emitted

`cpp/stack/src/stack_renderer.cpp` (`render_frame`, upstream's TODO). echion
pushes a dummy frame with line 0 carrying the task name; the renderer takes the
name and returns early.

### `TaskInfo::unwind` does not check for a running task

`cpp/stack/src/echion/tasks.cc`, upstream's TODO. Left as found.

### A failed `_PyCFrame` copy silently yields no stack on 3.11/3.12

`cpp/stack/src/echion/stacks.cc`, upstream's TODO. The unwinder returns instead
of signalling an invalid frame, so the sample is short rather than marked bad.
Fixing it means defining what that signal does to the renderer contract.

### `adapt_sampling_interval` casts before it clamps

`cpp/stack/src/sampler.cpp`. `static_cast<microsecond_t>(interval * (delta /
overhead))` runs before the min/max clamp below it, so a small enough
`target_overhead` divisor makes the conversion undefined. Closed from outside
instead: `pyroscope_stack_configure` rejects anything below
`g_min_target_overhead` (1e-4).

## Introduced by the port

### Truncated stacks are silent

`cpp/pyroscope/Pyroscope.h`. `Sample::push_frame` drops frames past
`max_nframes` and reports it through `incr_dropped_frames()`, a no-op in our
stats shim. The budget is configurable (`cpu_max_nframe`); only the reporting is
missing, and self-diagnostics are out of scope (`stack_scope.md`).

### Threads not created through `threading.Thread` are invisible

`rust/src/stack.rs` (`mod threads`). Registration hangs off
`Thread._set_native_id` / `_bootstrap_inner` plus a one-time sweep of
`threading._active`, so raw `_thread.start_new_thread` threads, C-created
threads and late `_DummyThread`s never register. The alternative -- filling the
map inside `for_each_thread` -- means `pthread_getcpuclockid` on a `pthread_t`
read out-of-band, a use-after-free if that thread has exited.

### `threading` stays patched after `shutdown()`

`rust/src/stack.rs` (`mod threads`). Once-per-process with no uninstall, as
upstream, so the wrappers keep registering with no agent running. Bounded by the
live thread count; consequence is that a non-empty map is not proof the agent is
up.

### `configure()` off the main thread leaves a stray `MainThread` entry

`Sampler::one_time_setup` registers `pthread_self()` as `"MainThread"`.
Harmless: `for_each_thread` looks entries up by `tstate.thread_id` and never
matches it.

### `pyroscope_stack_stop` skips upstream's two resets

`cpp/pyroscope/stack_ffi.cpp`. Upstream's `stack_stop` also runs
`ThreadSpanLinks::reset()` and `native_call_registry.reset()`; both are dead
code here because nothing populates either. Restore each with whichever feature
starts populating its structure.

### The cpu/wall profile is dropped when py-spy is also running

`rust/src/pyroscope.rs` (`PyroscopeAgent::snapshot`). Both sources publish
`__name__ = process_cpu`, so the stack sampler's profile is dropped with a
warning naming `cpu_enabled=False`. Renaming would cost the
`process_cpu:cpu:nanoseconds:cpu:nanoseconds` type ID the UI and the integration
tests use. The dump still runs, so the accumulator drains.

## Fragile invariants, no test

### The no-double-count rule for CPU time rests on two hoist loops

`cpp/stack/src/echion/threads.cc` (`unwind_tasks` and the greenlet loop).
`push_cputime` is conditional on `on_cpu` after a thread's first sample, so
avoiding double-counted CPU time depends on the on-CPU task or greenlet being
hoisted to index 0. The two loops use different bounds, and the greenlet one
keys off `snap.frame == Py_None`: if that sentinel changes, the swap no-ops and
CPU time doubles.

### `SampleManager::start_sample` assumes a single renderer thread

`cpp/dd_wrapper/include/sample_manager.hpp`. The `thread_local` stand-in for
upstream's `StaticSamplePool` is sound only because only the sampling thread
renders and `render_stack_end` always pairs `flush_sample()` with
`drop_sample()`. Neither is asserted.
