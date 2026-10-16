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

- The CPU timer engine cannot restart: `Engine::shutdown()` sets `stopped_once`, after which `Engine::start()` always returns `false`. Both `shutdown()` followed by `configure()` and `configure()` in a fork child (`at_fork_after_in_child` calls `Engine::shutdown()`) would otherwise report no CPU samples, so with the timer on `Sampler::start()` now fails when `Engine::start()` does, and `configure()` returns `False` with an error log.
- `Engine::postfork_child()` matches the surviving thread by native TID, which never matches in the child, so it unmaps that thread's profiler-owned alternate signal stack without disabling it; registering the thread again would reuse the dangling stack (`SIGSEGV` reproduced on Linux/Python 3.12). Unreachable while the engine cannot restart in a fork child; fix it before allowing that.
