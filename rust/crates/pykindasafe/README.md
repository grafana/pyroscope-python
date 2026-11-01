# pykindasafe

A local Apache-2.0 fork of kindasafe and kindasafe_init 0.1.1, combined into
one crate. See [UPSTREAM.md](UPSTREAM.md) for the pinned source and license.
Linux/macOS x86_64/aarch64 are supported source paths. Native recovery uses
assembly and OS signal contexts; it cannot execute under Miri.

## API and integration

Register `pykindasafe.workspace = true` in a consumer's dependencies. The base
workspace intentionally does not integrate the Python profiler.

- `unsafe init() -> Result<(), InitError>` installs recovery in ordinary thread
  context. Coordinate with other SIGSEGV/SIGBUS owners before installation.
  Failures latch permanently; a partial installation retains its fallback.
- `is_initialized() -> Option<Result<(), InitError>>` is a nonblocking status
  query. It does not detect subsequent foreign replacement.
- `check_recovery() -> Result<(), RecoveryError>` is signal-safe and preserves
  errno. It queries the **current calling thread's** mask, including masks
  inherited from enclosing handlers and the current handler's `sa_mask`.
- `unsafe u64(address)`, `unsafe slice(&mut destination, address)`, and
  `unsafe str(&mut destination, address)` return `ReadMemError`. Every source
  access checks recovery first; a null `str` source returns an empty string
  without accessing memory. `slice` errors from a recovered fault can leave
  partial output; unavailable recovery leaves the destination unchanged.
- `unsafe sanity_check()` is for ordinary thread context after initialization.
  It succeeds only for an actual recovered fault, never for a skipped read.

The read errors are `Fault { signal: u64 }`,
`RecoveryUnavailable(RecoveryError)`, `InvalidUtf8`, and `MissingNul`.
Recovery errors are `NotInitialized`, `SignalBlocked { signal: c_int }`, and
`MaskQueryFailed { error: c_int }`. Query errors use the returned pthread error
number, not an assumed errno value.

At signal-walk entry, skip the sample if `check_recovery()` fails. Continue to
propagate every read error: no preflight result is a reusable permission token.
Do not temporarily unblock SIGSEGV/SIGBUS and do not inspect only the saved
ucontext mask, which omits mask changes on handler entry. Retain the upper
ownership-check/failure-latching wrapper; initialization status cannot prove
that another component still leaves our handlers installed.

A worker that still calls these primitives must handle the same failures on its
own thread. If the worker moves to OS-mediated byte copies instead, that is a
separate fallible path: reject short copies and failed reads, validate copied
bytes, and do not create Rust references to the original foreign memory.
That path does not rely on this crate's signal recovery. It must not fall back
to unchecked native reads when OS copying fails.

## Foreign-memory obligations

See the public Rustdoc `# Safety` sections before any unsafe call. An address
may be stale, foreign, unaligned, or inaccessible; a live Rust source allocation
is not required. Readable bytes must nevertheless be initialized, observable
without forbidden aliasing or races, and distinct from the mutable destination.
Fault recovery establishes none of those conditions and proves no object type
or identity. On x86_64, copy lengths must fit in `u32` (the inherited assembly
counter limit); source ranges must not wrap.

For the signal walk, the consumer must justify why the interrupted thread owns
and stabilizes the inspected interpreter state, including nested contexts and
other threads. Blocking-mask checks do not supply that argument.

The existing detached worker's deferred CPython addresses cannot satisfy the
native-load contract just by adding unsafe blocks. Taking the GIL prevents
ordinary concurrent interpreter mutation but does not resurrect an object
freed or reused since sampling. Direct loads need both synchronization and
lifetime/identity evidence, such as retained objects indexed by stable IDs, or
metadata copied while lifetime is established. OS-mediated copies are an
alternative owned by the upper PR, not an integration completed in this base.

## Signal ownership and fallback limits

Fallback snapshots are immutable, atomically published before installation,
and retained for the process lifetime. Handlers do not acquire the initializer
lock. No POSIX operation atomically queries, publishes and installs a new
disposition relative to an uncoordinated foreign `sigaction` caller; applications
must coordinate these changes before installing and throughout protected reads.
An ownership check afterward cannot close that earlier race.

Software-generated deliveries respect ignore/default dispositions and the
previous callback's C signature. The kernel applies the previous mask and
NODEFER, ONSTACK and RESTART flags. A one-shot callback is consumed atomically
by its first fallback delivery, while recovered reads leave it available.
The installed disposition remains our trampoline, so OS introspection does not
show the logical one-shot state. Interposition cannot make ignored deliveries
invisible to every interrupted syscall or debugger. Previous callback code
must remain loaded, and callbacks must not change ownership during reads.

Default SIGSEGV/SIGBUS fallback restores the default disposition and redelivers
to the same thread. If that fails, the final fallback is `_exit(128 + signal)`.
Ignored hardware faults outside protected instructions retain the platform's
undefined behavior; ignore support concerns software-generated signals.
