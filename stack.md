# Stack CPU profiler

## Included

- cpu profiling; `oncpu=True` skips unwinding idle threads and uploads cpu only, `oncpu=False` uploads cpu+wall
- syscall memory copy
- fast copy (safe_memcpy), on by default after a 15 s warmup on the syscall copy; its SIGSEGV/SIGBUS handlers and the faulthandler patch install from `stack::start`, not at import, and only when fast copy is on
- thread registration via `threading`
- fork child and restart handling `configure_experimental_stack_profiler(fast_copy, fast_copy_warmup, max_nframe, max_threads, adaptive_*)`.
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
- A failing `faulthandler.enable()` leaves faulthandler disabled: the patch calls `disable()` before `enable()`, and faulthandler has no getter to restore the previous `file`/`all_threads`.
- The patched `faulthandler.enable()` wipes a foreign handler installed between two `enable()` calls on faulthandler's other signals, since `disable()` restores all five.
- `faulthandler.enable()` during the fast-copy warmup loses fast copy for the life of the process: the handler swap acts only while `fast_copy_active`, which is false during warmup.
- Embedded interpreters, and processes whose `/proc/self/exe` is unreadable, silently stay on the syscall copy.
- Sampler::stop() timeout leads to a data race against reset_string_cache / interner::clear 
- Thread registration releases the GIL, allowing thread ID reuse to overwrite a newer registration with a stale native ID and lose CPU samples on Linux; this port's startup snapshot widens the race window.
- Threads that pass `_set_native_id` before patch installation but are still in `threading._limbo` during the startup snapshot miss registration entirely and produce no CPU or wall samples.
