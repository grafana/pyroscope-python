# Stack CPU profiler

## Included

- cpu profiling; `oncpu=True` skips unwinding idle threads and uploads cpu only, `oncpu=False` uploads cpu+wall
- fast copy (safe_memcpy) only, with no syscall copy fallback and no warmup; its SIGSEGV/SIGBUS handlers install from `stack::start`, not at import, and `configure()` returns `False` when they cannot be installed
- no faulthandler patch: enable faulthandler before `configure()`; a later `faulthandler.enable()` replaces our handlers and the sampler stops
- thread registration via `threading`
- fork child and restart handling `configure_experimental_stack_profiler(max_nframe, max_threads, adaptive_*)`.
- adaptive sampling, on by default; `sample_rate` only sets the starting interval

## Intentionally excluded

- Labels and thread/task info
- dropped-frame counts
- `samples/count`
- native frames
- GC frames
- free-threaded CPython.

## Follow-ups

- asyncio/uvloop task unwinding and `max_tasks`
- gevent/greenlets.

## Known upstream issues that we're not fixing in this port
- A component that replaces our SIGSEGV/SIGBUS handlers after start stops the sampler, but a fault in `safe_memcpy` before the sampler loop sees the change reaches the foreign handler.
- Embedded interpreters also install our SIGSEGV/SIGBUS handlers on top of the host's.
- Sampler::stop() timeout leads to a data race against reset_string_cache / interner::clear 
- Thread registration releases the GIL, allowing thread ID reuse to overwrite a newer registration with a stale native ID and lose CPU samples on Linux; this port's startup snapshot widens the race window.
- Threads that pass `_set_native_id` before patch installation but are still in `threading._limbo` during the startup snapshot miss registration entirely and produce no CPU or wall samples.

## CPU timer profiling cherry-pick

Squashed from [DataDog/dd-trace-py#18724](https://github.com/DataDog/dd-trace-py/pull/18724) at `e16880aa5845d5c9688dbb31a80edf1d6d25a233` (submodule commit `2f90793c1`). Not in upstream `main` yet. Local changes in `cpu_timer.cpp`: `oncpu` is passed to `render_cpu_sample_begin`, and `Engine::postfork_child()` no longer re-arms the engine, since the child never restarts the sampler and `at_fork_after_in_child` shuts it down anyway.

### Known problems

- ASAN: the wall sampler, which still runs next to the timer, reads other threads' stale frames (heap-use-after-free in freed generators, stack-use-after-return on returned `entry` frames, both in `Frame::read`), and both the wall path and the timer's `drain_state` over-read short name strings in `pyunicode_to_utf8` (heap-buffer-overflow); the SIGPROF handler itself has no reports.
- CPU timer state consumes more than 500 KiB of resident memory per registered thread: every `CaptureState` embeds a 64-slot ring and every slot reserves space for 512 frames, regardless of `max_nframe` or `max_threads`. Because the timer is enabled by default, thread-heavy processes can consume hundreds of MiB solely for these rings (roughly 515 MiB at 1,000 threads).
- The CPU timer engine cannot restart: `Engine::shutdown()` sets `stopped_once`, after which `Engine::start()` always returns `false`. Both `shutdown()` followed by `configure()` and `configure()` in a fork child (`at_fork_after_in_child` calls `Engine::shutdown()`) would otherwise report no CPU samples, so with the timer on `Sampler::start()` now fails when `Engine::start()` does, and `configure()` returns `False` with an error log.
- Fixed: `Engine::postfork_child()` matches the surviving thread by native TID, which changes across fork, so it used to unmap that thread's profiler-owned alternate signal stack while it was still registered, and an unrelated `SA_ONSTACK` signal then crashed the child. Each live, retired and leaked mapping is now unmapped only if `sigaltstack` reports a registration outside it; the mapping the child still has registered, or every mapping if the query fails, is left mapped. Application-owned stacks are never touched. Remaining: the inherited stack stays registered and leaks in the child; the parent still leaks the stacks of threads retired from another thread; `disable_current_thread_altstack()` still ignores `sigaltstack` failures.
