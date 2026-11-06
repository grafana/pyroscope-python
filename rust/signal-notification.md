# Signal consumer notification

The consumer sleeps indefinitely when there are no samples. The first publication
wakes it to start a batching interval. It then waits for roughly half of the
65,536 ring slots (including record headers), or 100 ms, whichever comes first.
This gives isolated samples a delivery deadline without idle polling or a syscall
for every sample. Scheduling delays and name resolution can extend that deadline.
Occupancy is a hint: concurrent consumption can overestimate it and wake early.
The reporter still takes already resolved stacks from its existing shared buffer.

Each drain resolves at most 64 records. While records remain, the consumer starts
another batch immediately, checking shutdown between batches. Shutdown stops the
timers, removes the callback registration, waits for in-flight callbacks, and
drains the finite remainder. Resolution failures do not hide remaining records:
continuation depends on ring occupancy, not the number of resolved traces.

## Wakeup protocol

Each session owns a nonblocking pipe and an atomic threshold. Zero means the
consumer is active; one requests the first record; half capacity requests a batch.
After attempting to publish, every producer performs an acquire/release RMW on the
threshold, even when it is zero. If the occupancy meets a nonzero threshold, one
producer wins a CAS to zero and writes a byte. Other producers coalesce behind it.
No producer waits for another producer or for the consumer.

Before sleeping, the sole consumer clears old pipe bytes, exchanges the threshold,
and rechecks occupancy. All threshold changes are acquire/release RMWs, including
disarming after a wait. Thus publications preceding the arm are carried through
the RMW chain to the recheck; publications following the arm see its threshold.
A qualifying publication either appears in the recheck or leaves a readable pipe.
The consumer never clears the pipe between the recheck and poll. This ordering is
necessary: a load-only producer fast path or a plain store when disarming could
break the publication handoff. A producer delayed before its CAS can create an
extra wakeup but cannot erase a newer publication. Below-threshold records have
the batching deadline even if no more producers run.

The winning producer can be descheduled before writing. The consumer can wait for
that producer, but the producer never waits on consumer-owned state. It can also
interrupt the consumer safely. A one-byte nonblocking write cannot partially
succeed. EINTR retries the write; EAGAIN means a readable byte already exists.
The consumer retries interrupted reads and polls; timed polls retain the original
monotonic deadline rather than restarting the interval. Shutdown stores the stop
flag and unconditionally writes a byte, bypassing coalescing. The consumer checks
the flag after arming, so shutdown before the wait is also handled.

## Callback and descriptor lifetime

The static registry holds an atomic pointer and a reader count. A callback
increments the count before loading the pointer, uses the notifier and unwinder,
then decrements the count. All registry operations are sequentially consistent.
Unregistration clears the pointer before waiting for zero readers. In that total
order, a callback that saw the old pointer incremented before the clear, so the
wait cannot finish until it releases its reference. A callback arriving later
sees null and does no work. Only a normal thread waits; an interrupting callback
never waits on the thread it interrupted.

Registration owns an Arc, but callbacks neither clone nor drop it. The existing
exclusive signal-handler guard serializes registrations and remains owned until
after unregistration. This also handles startup failure after registration. Both
pipe ends remain owned while a callback or the shutdown caller can use them;
there is no close/reuse race or write with no reader. A restart uses a fresh pipe
and fresh threshold, without pending bytes from the previous session. Delayed
signals that arrive after a restart use the current registration. Descriptor
setup errors close both ends through normal-thread ownership cleanup.

## Signal-safety audit

The added callback path consists only of lock-free atomic operations, pointer
reads, integer arithmetic, a stack byte, and (when needed) native `write` plus
saving/restoring native `errno`. It performs no allocation, mutex/condition-variable
operation, logging, Rust TLS lookup, lazy initialization, Arc refcount operation,
or scheduler call. Pipe creation, fcntl, close, poll, clock reads, error formatting,
and the quiescence wait are all outside the handler. Recovery initialization and
successful kindasafe memory loads are unchanged; the existing recovery contract
in README.md and the unwinder crate remains in force.

* [Rust atomic portability](https://doc.rust-lang.org/std/sync/atomic/index.html#portability)
  guarantees that available standard atomic types are lock-free. Compilation
  requires pointer-width and byte atomics. The release targets (x86-64 and AArch64,
  Linux with either supported libc, and macOS) expose both through
  `rustc --print cfg --target ...`. The notification uses AtomicPtr, AtomicUsize,
  and the existing AtomicBool stop flag, never an emulated atomic wrapper.
  Lock-free does not promise a bound on hardware retry loops under contention.
* [Linux signal safety](https://man7.org/linux/man-pages/man7/signal-safety.7.html)
  specifies write and saved/restored errno access as async-signal-safe.
* [macOS signal safety](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/sigaction.2.html)
  lists write and advises preserving errno. `__errno_location` on Linux and
  `__error` on macOS access the libc thread's already existing errno slot; this
  does not introduce application TLS initialization.
* [Pipe semantics](https://man7.org/linux/man-pages/man7/pipe.7.html) and
  [macOS write semantics](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/write.2.html)
  specify nonblocking writes and their errors. The byte is smaller than PIPE_BUF.
  Both ends are kept alive, excluding EPIPE/SIGPIPE; valid owned descriptors and
  a stack byte exclude EBADF/EFAULT. Interposing an incompatible write or errno
  implementation, externally closing private descriptors, or asynchronous native
  thread cancellation is outside this protocol's guarantees.

## Regression coverage

Notification tests cover threshold crossing, concurrent coalescing, publication
before arming and between recheck/poll, the low-volume deadline, idle shutdown,
shutdown before arming, a full pipe and errno preservation, actual signal delivery and interrupted poll deadlines,
callback quiescence and repeated registration, and sustained concurrent producers
with bounded drain batches and exact record accounting. Platform execution and
profile-shape validation results are reported with the change.

Validation on Linux x86-64 with Python 3.13.7:

* `cargo test --locked --workspace`: 94 tests passed, including eight notification
  tests and a ring occupancy/index-wrap regression.
* `cargo clippy --locked --workspace --all-targets -- --deny warnings`,
  `cargo fmt --all --check`, and `git diff --check` passed.
* A local HTTP capture harness exercised both timer modes twice with real Python
  stacks. An isolated signal produced exactly the full sequence
  `<module>;low_volume;isolated`, with weight 1,000,000,000 at a 1 Hz rate.
  Sustained sampling used `<module>;sustained;burn`: assertions required at least
  90% of total weight on that complete sequence and 0.72–1.50 seconds of total
  sample weight for 1.2 seconds of work, allowing scheduling and profiling cost.
  The observed dominant share exceeded 97%. Assertion diagnostics retained all
  decoded profiles. The same harness passed 40 idle shutdown/restarts and the
  existing eight-worker, deep-stack, 1 kHz shutdown-under-load workload.
* Required atomic widths were checked for all six release targets. Native runtime
  execution here did not cover macOS, AArch64, musl, or other Python versions; the
  container integration matrix and sanitizer matrix were not run.
