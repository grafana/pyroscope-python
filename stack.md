# Stack CPU profiler

## Included

- cpu profiling, `oncpu=True` only: idle threads are not unwound and only cpu is uploaded; `oncpu=False` makes `configure()` return `False`
- fast copy (safe_memcpy) is mandatory, with no warmup and no syscall fallback; its SIGSEGV/SIGBUS handlers install from `stack::start`, not at import, and `configure()` returns `False` if they cannot be installed or, on a later `configure()`, if another handler has replaced them
- thread registration via `threading`
- fork child and restart handling `configure_experimental_stack_profiler(max_nframe, max_threads, adaptive_*)`.
- adaptive sampling, on by default; `sample_rate` only sets the starting interval

## Intentionally excluded

- wall profiling
- syscall memory copy (process_vm_readv / mach_vm_read_overwrite)
- the faulthandler patch: do not call `faulthandler.enable()` or `faulthandler.disable()` after `configure()`; a foreign SIGSEGV/SIGBUS handler stops the sampler until the process exits. `faulthandler.enable()` before `configure()` is fine
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
- Sampler::stop() timeout leads to a data race against reset_string_cache / interner::clear 
- Thread registration releases the GIL, allowing thread ID reuse to overwrite a newer registration with a stale native ID and lose CPU samples on Linux; this port's startup snapshot widens the race window.
- Threads that pass `_set_native_id` before patch installation but are still in `threading._limbo` during the startup snapshot miss registration entirely and produce no CPU or wall samples.
