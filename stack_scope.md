# CPU stack profiler: out of scope

What this port iteration deliberately does not do. An entry here is a decision,
not work: it does not belong in `stack_todo.md`, and taking one out of this file
is a decision to be asked for, not a cleanup. Pending work is in
`stack_todo.md`; defects we accept are in `stack_known_bugs.md`.

Format: one bullet, a sentence or two. Name what is inert and nothing more --
no write-up of how to implement it.

## Profile content

- **No labels, and no thread or task information.** `push_threadinfo`,
  `push_task_name`, `push_task_id`, `push_origin_task_id`,
  `push_origin_task_name`, `push_span_id`, `push_local_root_span_id`,
  `push_trace_type` and `push_monotonic_ns` are no-ops in
  `cpp/pyroscope/Pyroscope.h`.
- **No `samples/count` sample type.** `FFISampleValues` has no count slots and
  every call site passes a count of 1.

## Sampler features

- **No native monitoring.** `NativeCallRegistry::lookup` always returns
  `nullopt`, and upstream's `native_call_tracker.{hpp,cpp}` and `extern "C"`
  entry points are not ported.
- **No profiler self-diagnostics.** `ProfilerStats` and `ProfileBorrow`
  (`cpp/pyroscope/Pyroscope.h`) drop every argument, the fast-copy flags
  (`set_fast_copy_memory_enabled`, `_user_disabled`, `_capable`,
  `_syscall_fallback`) included. The sampler computes the numbers, but there is
  no sink to report them to -- upstream's is Datadog telemetry.
