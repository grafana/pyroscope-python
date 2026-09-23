# Vendored: dd-trace-py `stack` (echion sampler)

| | |
|---|---|
| Upstream | https://github.com/DataDog/dd-trace-py |
| Release | `v4.14.0` (commit `ef8651166784671cc6d5f82803c8ea38a0239557`) |
| Path | `ddtrace/internal/datadog/profiling/stack/` |
| License | Apache-2.0 OR BSD-3-Clause; `echion/` and `src/echion/` are MIT (see `LICENSE`) |

A wall-clock sampler: a dedicated thread wakes on an interval and walks every
Python thread, reading frames out of process memory with a fault-tolerant copy
primitive. Upstream called this `stack_v2` before 4.14.0.

**Linux, CPython 3.11+ only** -- see the thread auto-registration patch below.
The tree compiles on macOS and on 3.10, it just cannot discover threads there.

Driven from Rust over the C ABI in `src/pyroscope_entry.cpp`; see
`rust/src/cpu/native.rs`.

## No libdatadog

Upstream renders samples through `StackRenderer` -> `ddup_*` ->
`libdd_wrapper.so` -> Rust **libdatadog**, which owns pprof encoding and
upload. **None of that chain is vendored.** Specifically dropped:

- the entire `dd_wrapper/` tree,
- `src/stack_renderer.cpp` and `include/stack_renderer.hpp`, replaced by
  `src/pyroscope_renderer.cpp` + our own `include/stack_renderer.hpp`, which
  render into `Pyroscope::CpuSample` (`../PyroscopeCpu.h`) and push over the C
  ABI in `rust/include/pyroscope_ffi.h`.

`libdd_wrapper.so` is a pre-built binary artifact and is not buildable from
source here; taking it would also mean two pprof encoders and two uploaders in
one process.

`shim/dd_wrapper/include/` holds small replacements for the few headers the
retained code still includes -- see the header comments in each. Only
`constants.hpp`, `defer.hpp` and `scope.hpp` are genuine upstream files (pure
utility code, no libdatadog dependency); `sample.hpp`, `profiler_stats.hpp`
and `profiler_state.hpp` are Pyroscope no-op shims. Keeping them as shims is
what lets `src/sampler.cpp` and `src/echion/stacks.cc` stay byte-identical to
upstream.

Check after building that no real dd_wrapper header leaked in:

```
nm --defined-only -g libpyroscope_cpu_ddtrace.a | grep -E 'ddog_|libdatadog'   # empty
nm -u _native.abi3.so | grep ddog_                                            # empty
```

## Other things dropped

- All `*.py` / `*.pyi`.
- `src/stack.cpp` -- the CPython extension-module glue (`PyMethodDef`
  `stack_start`/`stack_stop`/`register_thread`/`link_span`/...). Replaced by
  `src/pyroscope_entry.cpp`, a plain C ABI.
- Span linking, greenlet tracking, asyncio task attribution and
  `sys.monitoring` native-call tracking. All are Datadog product features
  driven from the Python layer, and none affects CPU sampling.
  `src/thread_span_links.cpp` and `src/origin_task_links.cpp` are still
  compiled because `src/sampler.cpp` references them; with nothing populating
  them they are inert.
- `fuzz/` and `test/`.
- `profiling_helpers/`, which `src/echion/frame.cc` includes, is **not**
  vendored again: `cpp/profiling_helpers/` (used by the memalloc profiler) is
  byte-identical to this release's copy, and `CMakeLists.txt` puts `cpp/` on
  the include path. One definition on disk means the two archives cannot drift
  into an ODR violation on those `namespace DataDog` inline functions.

## Local modifications

Marked in-file with `Pyroscope patch:` comments.

1. **`src/echion/threads.cc`, `for_each_thread()`** -- auto-register threads on
   discovery, and refresh entries whose thread has been replaced.

   Upstream depends on the Python `threading` patch calling
   `stack_thread_register()` for every thread. That call does two things:
   creates the `ThreadInfo`, and *overwrites* any existing entry under the same
   key. Both halves matter, and without the Python layer both have to happen
   here.

   - Without the insert, `thread_info_map` stays empty and the loop's
     `continue` silently skips **every** thread -- the profiler yields nothing.
   - Without the overwrite, recycled `pthread_t` values are mis-attributed. The
     map is keyed by `pthread_t`, which the C library reuses after a thread
     exits; a new thread inheriting a dead one's key would keep the dead
     thread's `cpu_clock_id` (derived from the old kernel TID), so
     `update_cpu_time()` reads a clock that no longer exists and the CPU delta
     stays zero for that thread's whole life. A thread-churning workload
     reported **5% of the CPU it actually used** before this was handled. The
     kernel TID (`PyThreadState::native_thread_id`) is the discriminator.

   Cost: no Python-level thread name (not reachable off-GIL from here), so this
   profiler reports no `thread_name` tag. Entries for dead threads are never
   evicted, which is fine because `pthread_t` reuse bounds the map at roughly
   the peak concurrent thread count rather than the total ever created.

   The patched path is compiled only for `PL_LINUX && PY_VERSION_HEX >= 3.11`.
   `PyThreadState::native_thread_id` does not exist before CPython 3.11, and
   only Linux can derive a usable per-thread CPU clock from it. Everywhere else
   the tree still compiles (the lookup miss falls through to upstream's
   `continue`) but yields nothing, so `CpuProfiler::check_supported` in
   `rust/src/cpu/mod.rs` refuses to start it rather than report a blank profile:
   non-Linux is rejected because `rust/build.rs` does not build the archive
   there, CPython 3.10 by an explicit version check.

   **TODO(macos):** `ThreadInfo::create()` on Darwin calls
   `pthread_mach_thread_np()` on a `pthread_t` copied out of a concurrently
   changing interpreter list, which is undefined for a stale value. The likely
   fix is to use `native_id` directly (CPython sets `native_thread_id` from
   `pthread_mach_thread_np(pthread_self())` on macOS, so it is already the mach
   port) and let `update_cpu_time()`'s `thread_info()` call fail cleanly for a
   dead port -- needs verifying against CPython 3.11-3.14 first.

2. **`src/echion/vm.cc`, `echion/echion/vm.h`** -- dropped
   `__attribute__((constructor))` from `init_safe_copy()` and renamed it to
   `pyroscope_init_safe_copy()`.

   Upstream runs it at `dlopen`, i.e. on `import pyroscope`, installing
   process-wide SIGSEGV/SIGBUS handlers and a 1 MiB alt stack whether or not
   this profiler is ever selected. `src/pyroscope_entry.cpp` calls it
   explicitly (under a `std::once_flag`) when the sampler starts instead.

   Deliberately never uninstalled on stop: the handler chains to the previous
   disposition for anything it does not own, so leaving it installed is benign,
   whereas tearing it down while `fast_copy_active` stays true would leave
   `safe_memcpy` without its recovery path on a subsequent restart
   (`Sampler::sampling_thread` only reinstalls under its own `std::once_flag`).

3. **`src/sampler.cpp`, `stack_atfork_child()`** -- do not restart the sampler
   in the fork child.

   Upstream calls `Sampler::restart_after_fork()` there. Pyroscope's fork
   policy is the opposite: `rust/src/lib.rs::at_fork_after_in_child` stops
   profiling and deliberately leaks the agent, because the agent's threads did
   not survive the fork. An auto-restarted sampler would keep pushing samples
   into a Rust sink that no uploader is draining.

   The cleanup itself is kept, and kept on `pthread_atfork` rather than moved
   to Rust: it re-inits echion's mutexes and maps with placement new, and
   `pthread_atfork` also covers a raw `fork(2)` from C, which
   `os.register_at_fork` would miss. Because that handler already ran the
   cleanup, `pyroscope_cpu_ddtrace_postfork_child()` must not call
   `Sampler::postfork_child()` again.

## Sample weighting

This is a **wall-clock** sampler: it wakes on an interval and walks every
Python thread, producing one stack per thread per tick whether or not that
thread ran. `src/pyroscope_renderer.cpp` therefore weights each sample by the
per-thread CPU delta echion supplies via `render_cpu_time()`, and drops samples
from threads that consumed no CPU. `rust/src/cpu/mod.rs` tags the batch
`ReportData::ReportsCpuNanos` so `encode::pprof` writes those nanoseconds
verbatim instead of multiplying by the sampling period.

Weighting by the sampling period instead (one tick = one period of CPU, which
is correct for py-spy with `gil_only`) inflates the profile by roughly the
thread count and counts blocked threads as busy: 13.6s of "CPU" was measured
for a process that used 3.7s.

## Configuration notes

- `Sampler::set_interval()` takes **fractional seconds**, not microseconds.
- The stack depth cap lives in echion's `max_frames` global (`echion/config.h`),
  not on `Sampler`.
- Adaptive sampling is deliberately **disabled** in `pyroscope_entry.cpp`. It
  varies the interval to hit a CPU-overhead target, which would make the sample
  rate the user asked for a suggestion rather than a setting and quietly trade
  samples for overhead under load.
- `UNWIND_NATIVE_DISABLE` and `PL_LINUX`/`PL_DARWIN` compile definitions are
  required, mirroring upstream's `stack/CMakeLists.txt`. Without `PL_*`,
  `echion/danger.h` compiles out its platform memory-copy primitives and the
  whole tree fails to resolve.
- `_POSIX_C_SOURCE` is deliberately **not** defined (unlike
  `cpp/CMakeLists.txt`): on macOS it selects the strict POSIX namespace, which
  hides BSD extensions the vendored code uses -- `echion/danger.cc` calls
  `getpagesize()`.

## Re-vendoring

`scripts/check-cpu-profilers-multiversion.sh` compiles this tree against
CPython 3.10-3.14 in about a minute; run it after any update, because that is
where version-specific CPython internals break.
