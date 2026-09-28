# CPU stack profiler: known bugs we are not fixing

Defects in the shipped CPU sampler (`cpp/stack/`, `cpp/dd_wrapper/`,
`cpp/pyroscope/stack_ffi.cpp`, `rust/src/stack.rs`) that are known, reproducible
or at least understood, and deliberately left in place. Each entry says what
goes wrong and why the fix is not worth taking now.

This is not a work list. `stack_todo.md` holds outstanding work, unimplemented
features (labels, native frames, adaptive sampling) and build policy. A bug
that we decide to fix moves out of here and into that doc.

Two reasons recur, so they are named once here:

- **Verbatim from upstream.** The port's whole strategy is to keep upstream's
  files, names and call shapes so the next vendor sync stays a small diff
  (`CLAUDE.md`). A local fix to vendored logic is a conflict we carry forever,
  against code Datadog is still changing. The bar for taking one is that the bug
  hurts us specifically, or corrupts data.
- **First-iteration scope.** The branch's goal was to make the sampler run at
  all. Anything needing a new mechanism to fix is out of scope until the shape
  settles.

## Verbatim from upstream

### A failing `faulthandler.enable()` leaves faulthandler disabled

`rust/src/stack.rs:305`. The wrapper runs `_original_disable()` before
`_original_enable(*args)`, and the exception path only reinstalls our SIGSEGV
handler before re-raising. So `faulthandler.enable(file=closed_file)` raises
`ValueError` and drops crash reporting that was active before the call;
unpatched CPython validates its arguments first and leaves the previous
configuration untouched. Reproduced on 3.14.5/macOS: `is_enabled()` goes
`True` -> `False`.

Not fixed because the restore is not implementable faithfully. There is no
getter for faulthandler's `file` or `all_threads`, so re-enabling after the
failure would silently move crash output to stderr and change `all_threads`,
which is a different and quieter bug than the one it replaces. Upstream's
`ddtrace/profiling/_faulthandler.py` makes the same call.

### `faulthandler.enable()` clobbers foreign handlers on its other signals

`rust/src/stack.rs:305`. The `disable()` + `enable()` pair exists to stop
faulthandler recording itself as its own previous handler, but `disable()`
restores the saved handler for *all* of faulthandler's signals (SIGSEGV, SIGFPE,
SIGABRT, SIGBUS, SIGILL), not just the one we care about. A handler installed
between two `enable()` calls is therefore wiped. Unpatched `enable()` is a no-op
on an already-enabled faulthandler and preserves it. Reproduced: with a Python
SIGABRT handler installed in between, the second `enable()` makes
`raise_signal(SIGABRT)` abort the process with faulthandler's fatal-error dump
instead of running the handler.

Not fixed in the vendored logic, for the reason above. Note one narrowing that
*is* ours to make and is written up in `stack_todo.md`: with `cpu_fast_copy`
off, no SIGSEGV handler exists to protect, the C++ `uninstall`/`reinstall`
already no-op on `fast_copy_active` (`cpp/pyroscope/stack_ffi.cpp:63,71`), and
the swap is pure downside, so gating `faulthandler::install`
(`rust/src/stack.rs:87`) on fast copy removes the exposure for the default
configuration without touching upstream's body.

### `enable()` during the fast-copy warmup loses fast copy permanently

`cpp/pyroscope/stack_ffi.cpp:63,71`. `uninstall_segv_handler` and
`reinstall_segv_handler` act only `if (fast_copy_active)`, and
`sampling_thread` holds that false for the whole warmup. A
`faulthandler.enable()` inside the warmup window therefore skips both swaps,
faulthandler lands on top of our handler, and when warmup ends
`segv_handler_installed()` is false, so the process stays on the syscall copy
for good.

Ported as-is (upstream main a1bcb762 does the same) and pinned by
`enable_during_warmup_falls_back` in
`integration-test/testdata/sighandler_workload.py`, so a fix flips a test. The
fix is to gate on "fast copy requested and `safe_memcpy_initialized`" rather
than on `fast_copy_active`. Low impact: the penalty is the slower copy path,
not wrong data.

### Thread CPU time is reported as task CPU time

`cpp/stack/src/stack_renderer.cpp:221`, carrying upstream's own comment that
this is "absolutely false". The per-thread CPU delta is pushed for the task
sample. Kept because it is how upstream's v1 sampler behaves and normalizing to
the task level is upstream's open work, not ours.

### Wall time is multiplied by the leaf task count

`cpp/stack/src/echion/threads.cc`, `cpp/stack/src/stack_renderer.cpp`.
`ThreadInfo::sample` renders one sample per leaf asyncio task or greenlet stack,
and each one pushes `push_walltime(thread_state.wall_time_ns, 1)` with the same
per-cycle thread delta. A thread with 50 live tasks contributes 50x the elapsed
wall time for that cycle.

This is upstream's intended per-task attribution, so it stays. The consequence
worth knowing: the `wall` total is not conservative and must never be
sanity-checked against wall-clock elapsed time.

### The task credited as on-CPU may not be the one that was on CPU

`cpp/stack/src/echion/threads.cc:266`. The thread stack is captured
out-of-band, so the task believed to be on CPU can differ from the one running
when the frames were read, which mis-splices the coroutine and sync halves of
the stack. Upstream reports never observing it; the fix it suggests is matching
every task stack against the thread stack, which costs real work per sample.

### `TaskInfo::unwind` does not check for a running task

`cpp/stack/src/echion/tasks.cc:206`, upstream's TODO. Left as found.

### A failed `_PyCFrame` copy silently yields no stack on 3.11/3.12

`cpp/stack/src/echion/stacks.cc:95`. When `copy_type` of the `_PyCFrame`
fails, the unwinder returns instead of signalling an invalid frame, so the
sample is silently short rather than marked bad. Upstream's TODO; fixing it
means defining what an invalid-frame signal does on our side, which touches the
renderer contract.

### Stacks deeper than 64 frames lose their outermost frames, silently

`cpp/dd_wrapper/include/sample_manager.hpp:48` fixes `max_nframes` to
`g_default_max_nframes` (64), and `Sample::push_frame`
(`cpp/pyroscope/Pyroscope.h:164,182`) drops the overflow and calls
`incr_dropped_frames()`, which is a no-op in our stats shim
(`cpp/pyroscope/Pyroscope.h:277`). So deep stacks are truncated and nothing
reports it.

Upstream plumbs `SampleManager::set_max_nframes` from Python config (clamped to
512) through the Cython layer we do not vendor, so exposing it is new plumbing
rather than a fix: it should follow `configure(mem_max_nframe=...)`.

## Introduced by the port

### The fork child resurrects the sampler, then samples with a cleared interner

`rust/src/ffikit.rs:93` (existing `TODO(Pyroscope)`), `cpp/stack/src/sampler.cpp`.
`stack_atfork_child` runs inside `os.fork()`, before Python's
`at_fork_after_in_child` reaches `stop_profilers`, and it calls
`restart_after_fork()`. The sampling thread is therefore live and re-warming
`StackRenderer::string_id_cache` while we clear the string table underneath it,
so the child can push samples keyed by stale string indices.

Unfixed because the correct behaviour is to keep the sampler stopped in the
child (the agent is dead there), and that has to be done together with the next
entry. `renderer_.postfork_child()` is not a substitute.

### The fork child never re-registers its own `MainThread`

The child inherits the parent's thread info map and the inherited `threading`
patch, so nothing registers the child's main thread.
`Sampler::postfork_child()` rebuilds both the map and that entry but has no
caller. Same reason as above: fork safety is one piece of work, not two.

### Threads not created through `threading.Thread` are invisible

`rust/src/stack.rs` (`mod threads`). Registration hangs off
`Thread._set_native_id` / `Thread._bootstrap_inner` plus a one-time sweep of
`threading._active`, so raw `_thread.start_new_thread` threads, C-created
threads that attach later, and `_DummyThread`s appearing after the sweep never
register and are never sampled. Inherited from upstream's `init_stack`.

Not fixed because the alternative is filling the map from the tstate snapshot
inside `for_each_thread`, which means calling `pthread_getcpuclockid` /
`pthread_mach_thread_np` on a `pthread_t` read out-of-band: a use-after-free if
that thread has exited.

### `threading` stays patched after `shutdown()`

`rust/src/stack.rs` (`mod threads`). The patch is once-per-process with no
uninstall, matching upstream, so the wrappers keep calling
`register_thread`/`unregister_thread` with no agent running and the map stays
live between sessions. Bounded by the live thread count, since unregistration
still fires on thread exit. Consequence: `pyroscope_stack_thread_count()` is
not zero between sessions, so a non-empty map is not proof the agent is up.

### `configure()` off the main thread leaves a stray `MainThread` entry

`one_time_setup`'s `postfork_child` registers `pthread_self()` as
`"MainThread"`. If `configure()` runs on some other thread, that entry is keyed
by a non-Python thread id. Harmless in practice: `for_each_thread` looks entries
up by `tstate.thread_id` and never matches it. Not worth a guard until something
depends on the map being exact.

### `pyroscope_stack_stop` skips upstream's two resets

`cpp/pyroscope/stack_ffi.cpp:43`. Upstream's `stack_stop` also runs
`ThreadSpanLinks::reset()` and `native_call_registry.reset()`; both are dead
code here because nothing populates either (no span hook, stub registry).
Deliberately omitted, and a trap for later: **restore each reset with whichever
feature starts populating its structure.**

### The cpu/wall profile is dropped when py-spy is also running

`rust/src/pyroscope.rs:294`. Both sources publish `__name__ = process_cpu`, so
`PyroscopeAgent::snapshot` drops the stack sampler's profile with a warning
naming `cpu_enabled=False` as the way out. Chosen over renaming, which would
cost the `process_cpu:cpu:nanoseconds:cpu:nanoseconds` type ID the UI and the
integration tests already use. The dump still runs each window so the
accumulator drains instead of growing.

## Fragile invariants, no test

### The no-double-count rule for CPU time rests on two hoist loops

`cpp/stack/src/echion/threads.cc:229,694`. `push_cputime` is unconditional on a
thread's first sample and conditional on `on_cpu` afterwards, so avoiding
double-counted CPU time depends entirely on the on-CPU task or greenlet being
hoisted to index 0, where `render_task_begin` reuses the already-credited
sample. The hoist is written twice with different loop bounds (asyncio: `i = 0`
with an `if (i > 0)` guard; greenlets: `i = 1`), and the greenlet one keys off
`snap.frame == Py_None`, greenlet's sentinel for the currently-running
greenlet. If that sentinel ever changes, the swap silently no-ops and CPU time
doubles.

Vendored logic, and the failure is a plausible-looking 2x rather than a crash,
which is exactly why it is written down here.

### `SampleManager::start_sample` assumes a single renderer thread

`cpp/dd_wrapper/include/sample_manager.hpp:48`. The `thread_local` single-
instance replacement for upstream's `StaticSamplePool` is sound only because
(a) only the sampling thread renders and (b) `render_stack_end` always pairs
`flush_sample()` with `drop_sample()` before the next `start_sample()`. Both
hold today; neither is asserted. If a second renderer thread ever appears,
revisit this before debugging the resulting corruption.
