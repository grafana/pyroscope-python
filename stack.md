# Stack CPU profiler

## Included

- cpu+wall cpu profiling 
- syscall memory copy
- thread registration via `threading`
- fork child and restart handling `configure_experimental_stack_profiler(max_nframe, max_threads)`.

## Intentionally excluded

- Labels and thread/task info
- dropped-frame counts
- `samples/count`
- native frames
- GC frames
- free-threaded CPython.

## Follow-ups

- wire oncpu flag into the new profiler
- Fast copy (safe_memcpy, SIGSEGV/SIGBUS handlers, faulthandler patch, warmup)
- adaptive sampling
- asyncio/uvloop task unwinding and `max_tasks`
- gevent/greenlets.

## Known upstream issues that we're not fixing in this port
- Sampler::stop() timeout leads to a data race against reset_string_cache / interner::clear 
- Thread registration releases the GIL, allowing thread ID reuse to overwrite a newer registration with a stale native ID and lose CPU samples on Linux; this port's startup snapshot widens the race window.
