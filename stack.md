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
