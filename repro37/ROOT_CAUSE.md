# grafana/pyroscope-python#37 — root cause

Random native crashes (`free(): invalid pointer`, SIGSEGV / exit 139) in
processes running the Pyroscope Python agent. Reported against 0.8.11/0.8.14
(celery prefork) and again against 1.2.1 (gunicorn sync workers, Python 3.11).

**TL;DR** — py-spy materializes a `_PyInterpreterFrame` out of bytes read from
the profiled process with `ptr::read::<T>()`. On **Python 3.11** that struct
contains `is_entry: bool`, and rustc uses that `bool` as the *niche
discriminant* of the returned `Result<_PyInterpreterFrame,
remoteprocess::Error>`. When the sampler reads a stale frame whose byte 68 is
`2`, the `Ok` silently becomes an `Err` whose payload is the 32 interpreter
bytes just read. That bogus error travels to pyroscope's consumer thread, and
dropping it calls `free()` on whatever those bytes contain.

* Not an ABI mismatch (the original hypothesis in the issue).
* Not memory profiling (this predates it; everything here runs CPU-only).
* Not heap corruption caused by pyroscope's own bookkeeping — the heap is
  intact right up to the bogus `free()`.

---

## 1. The symptom

Both reporters' stacks are the same, and identical to what reproduces here:

```
#0  __pthread_kill_implementation
#3  <signal handler called>
#4  __GI___libc_free (mem=0x18)                       <- freeing "0x18"
#5  anyhow::error::object_drop ()      from .../pyroscope/_native...so
#6  std::sys::backtrace::__rust_begin_short_backtrace ()
#7  core::ops::function::FnOnce::call_once{{vtable.shim}} ()
#8  std::sys::pal::unix::thread::Thread::new::thread_start ()
```

The crashing thread is pyroscope's py-spy consumer thread — the one running
`for sample in sampler_output` in `Pyspy::initialize` (`rust/src/pyspy_backend.rs`).
The two nested `object_drop` frames are the `anyhow::Error` chain inside
`Sample.sampling_errors`.

## 2. Root cause

### 2.1 The unsound conversion

`remoteprocess-0.5.2/src/lib.rs`:

```rust
fn copy_struct<T: Copy>(&self, addr: usize) -> Result<T, Error> {
    let mut data = vec![0; std::mem::size_of::<T>()];
    self.read(addr, &mut data)?;
    Ok(unsafe { std::ptr::read(data.as_ptr() as *const _) })
}

fn copy_pointer<T: Copy>(&self, ptr: *const T) -> Result<T, Error> {
    self.copy_struct(ptr as usize)
}
```

`T: Copy` does **not** mean "every bit pattern is a valid `T`". py-spy's
Python 3.11 binding is:

```rust
// py-spy/src/python_bindings/v3_11_0.rs
pub struct _PyInterpreterFrame {
    pub f_func: *mut PyFunctionObject,      // 0
    pub f_globals: *mut _object,            // 8
    pub f_builtins: *mut _object,           // 16
    pub f_locals: *mut _object,             // 24
    pub f_code: *mut PyCodeObject,          // 32
    pub frame_obj: *mut _frame,             // 40
    pub previous: *mut _PyInterpreterFrame, // 48
    pub prev_instr: *mut u16,               // 56
    pub stacktop: i32,                      // 64
    pub is_entry: bool,                     // 68   <-- niche
    pub owner: c_char,                      // 69
    pub localsplus: [*mut _object; 1],      // 72
}                                           // size 80
```

A Rust `bool` may only hold `0` or `1`. It is the **only** niche in the struct
(raw pointers are nullable, integers and `c_char` have no niche), so rustc
uses it for the `Result` discriminant — and there is no room for anything else:

```
(gdb) p sizeof('py_spy::python_bindings::v3_11_0::_PyInterpreterFrame')
$1 = 80
(gdb) p sizeof('core::result::Result<py_spy::python_bindings::v3_11_0::_PyInterpreterFrame, remoteprocess::Error>')
$2 = 80
```

Same size ⇒ no separate tag ⇒ `Err` must be encoded inside the payload.

### 2.2 Disassembly: `Err` is written into the `is_entry` byte

Shipped wheel, `pyroscope-io==1.2.1`,
`pyroscope/_native.cpython-311-x86_64-linux-gnu.so` (not stripped):

```asm
0000000000900f80 <remoteprocess::ProcessMemory::copy_pointer::hfa7f1a9f30c1a49b>:
  ...
  900fbb: mov    $0x50,%r8d              ; len = 0x50 = 80 = sizeof(_PyInterpreterFrame)
  900fc1: mov    %r12,%rsi               ; self
  900fc4: mov    %r15,%rdx               ; addr
  900fc7: mov    %rax,%rcx               ; the vec![0; 80] buffer
  900fca: call   *0x29c460(%rip)         ; Process::read(addr, &mut data)
  900fd0: cmpl   $0xf,0x8(%rsp)          ; did read() succeed?
  900fd5: jne    901004                  ;   no  -> Err path
                                         ;   yes -> Ok path:
  900fd7: movups 0x40(%r14),%xmm0        ; copy all 80 bytes of target memory
  900fdc: movups %xmm0,0x40(%rbx)        ;   into the returned Result,
  900fe0: movups (%r14),%xmm0            ;   byte 0x44 included, unvalidated
  900fe4: movups 0x10(%r14),%xmm1
  900fe9: movups 0x20(%r14),%xmm2
  900fee: movups 0x30(%r14),%xmm3
  900ff3: movups %xmm3,0x30(%rbx)
  900ff7: movups %xmm2,0x20(%rbx)
  900ffb: movups %xmm1,0x10(%rbx)
  900fff: movups %xmm0,(%rbx)
  901002: jmp    901019
  901004: movups 0x8(%rsp),%xmm0         ; Err path: 32-byte remoteprocess::Error
  901009: movups 0x18(%rsp),%xmm1        ;   into the first 32 bytes ...
  90100e: movups %xmm1,0x10(%rbx)
  901012: movups %xmm0,(%rbx)
  901015: movb   $0x2,0x44(%rbx)         ; <<<< discriminant := 2, stored in is_entry
  901019: mov    $0x50,%esi              ; free the temp buffer
  ...
```

So **`Err` == "byte 68 of the frame is 2"**, and the `Ok` path copies byte 68
straight out of the profiled process.

### 2.3 Disassembly: the caller tests that same byte

Same binary, `py_spy::stack_trace::get_stack_trace::h79a433b91fc99555`
(this is `let frame = process.copy_pointer(frame_ptr).context("Failed to copy PyFrameObject")?`):

```asm
  904232: lea    0x100(%rsp),%rdi        ; sret slot for the Result
  90423a: mov    %r15,%rsi
  904245: call   900f80 <...copy_pointer...>
  90424a: movzbl 0x144(%rsp),%eax        ; load ONE byte at slot+0x44 (0x144-0x100)
  904252: cmp    $0x2,%al                ; is it the Err niche value?
  904254: je     904be1                  ;   yes -> error path
  90425a: mov    %al,0xb(%rsp)           ;   no  -> keep it as `is_entry`
```

For contrast, the very next call in the same function copies a
`PyCodeObject` — which has no niche — and gets a real 32-bit tag, so it cannot
be confused:

```asm
  90428c: call   8fffd0 <...copy_pointer::h194f4e6da92eb3c4>   ; PyCodeObject, 184 bytes
  904291: cmpl   $0x1,0x100(%rsp)        ; full 32-bit tag at offset 0 of the Result
  904299: je     904c1f                  ; -> error path
```

`sizeof(Result<PyCodeObject, Error>) > sizeof(PyCodeObject)` there, so the tag
is its own word and no bit pattern of the copied struct can fake an `Err`.

The same code is in a from-source build of `main`
(`remoteprocess::ProcessMemory::copy_pointer<..., v3_11_0::_PyInterpreterFrame>`,
`movb $0x2,0x44(%rbx)` at `+104`, caller `movzbl -0x1ac(%rbp)` / `cmp $0x2`),
so this is not a quirk of one compiler run.

### 2.4 Why `c_char` is not equivalent (same layout, different validity)

`bool` and `c_char` (= `i8` here) have **identical size and alignment** — one
byte each. The 3.11 frame even has both, adjacent:

```rust
    pub is_entry: bool,      // offset 68
    pub owner: c_char,       // offset 69
```

What differs is the *validity invariant*, i.e. which bit patterns are legal
values:

| type | size | valid values | spare (niche) values |
|---|---|---|---|
| `bool` | 1 | `0`, `1` | 254 |
| `c_char` / `i8` / `u8` | 1 | all 256 | none |

rustc only needs a separate discriminant when it cannot hide one in a spare
value of the payload. So the *struct* layout is the same either way, but the
layout of the enum **wrapping** it is not:

```
                                       sizeof(frame)   sizeof(Result<frame, remoteprocess::Error>)
py_spy::python_bindings::v3_11_0        80              80     <- equal: discriminant hidden in `is_entry`
py_spy::python_bindings::v3_12_0        80              88     <- +8: dedicated tag word
py_spy::python_bindings::v3_14_0        88              96     <- +8: dedicated tag word
```

The generated code for a non-3.11 frame shows the dedicated tag (same build,
same function, different type parameter):

```asm
remoteprocess::ProcessMemory::copy_pointer<..., v3_14_0::_PyInterpreterFrame>:
  +30:  mov    $0x58,%edi           ; 0x58 = 88 = sizeof(v3_14_0::_PyInterpreterFrame)
  +77:  call   *...                 ; Process::read
  +83:  cmpl   $0xffffffff,-0x40(%rbp)
  +87:  je     +112                 ; -> Ok
  ; Err:
  +89:  movups -0x40(%rbp),%xmm0    ; remoteprocess::Error -> payload area at 0x08
  +101: movups %xmm0,0x8(%rbx)
  +105: mov    $0x1,%eax            ; tag = 1
  +110: jmp    +166
  ; Ok:
  +120: movups 0x40(%r14),%xmm0     ; 88 copied bytes -> payload area at 0x08..0x60
  ...
  +164: xor    %eax,%eax            ; tag = 0
  +166: mov    %rax,(%rbx)          ; <<<< tag stored in its OWN word at offset 0
```

The payload begins at offset `0x08` and the tag is a word of its own, so no
bit pattern of the copied struct can influence it. Contrast 3.11, where the
payload occupies offsets `0x00..0x50` — the tag *is* one of the copied bytes:

```asm
  901015: movb   $0x2,0x44(%rbx)    ; Err lives inside the payload
```

This is also why the fix in §8 is a one-line change: swapping `bool` for `u8`
does not move a single field, it only removes the spare values that rustc is
allowed to steal.

### 2.5 `#[repr(C)]` does not prevent this

py-spy's bindings are `#[repr(C)]` (bindgen emits it), which is worth being
explicit about because it is a natural objection:

```rust
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct _PyInterpreterFrame { ... }
```

`repr(C)` pins *that struct's* layout — field order, offsets, size,
alignment — so the C ABI is matched. It does **not** change its fields'
validity invariants (a `bool` is still only `0` or `1`), and it does not
constrain the layout of an enum that happens to wrap it. `Result` is a plain
`repr(Rust)` enum, and rustc's niche search descends into the payload's
fields recursively regardless of their `repr`. If anything, `repr(C)` makes
the niche's position *stable and predictable* (byte 68, always).

`niche_demo.rs` in this directory demonstrates it with no py-spy involved —
two `repr(C)` structs of identical layout, differing only in `bool` vs `u8`:

```
$ rustc -O -o /tmp/niche_demo niche_demo.rs && /tmp/niche_demo
size_of::<Err32>() = 24 (fits inside the Ok payload)

repr(C), field offsets identical in both structs:
  size_of::<FrameWithBool>()               = 80
  size_of::<FrameNoBool>()                 = 80

but the enum wrapping them is laid out differently:
  size_of::<Result<FrameWithBool, Err32>>() = 80  <- niche: tag hidden in the bool
  size_of::<Result<FrameNoBool,   Err32>>() = 88  <- dedicated tag word

Ok(frame) with byte 68 == 2 is observed as: Err  <-- the bug
```

Two conditions have to hold for the niche to be chosen at all, and both hold
here: the payload must contain a niche (`is_entry`), and the other variant
must fit in the remaining space (`remoteprocess::Error` is 32 bytes, the frame
is 80).

### 2.4 Why the bogus `Err` is fatal

py-spy adds a context and ships the error to pyroscope:

```rust
// py-spy/src/stack_trace.rs
let frame = process.copy_pointer(frame_ptr).context("Failed to copy PyFrameObject")?;
// ... propagated up through
//     .with_context(|| format!("Failed to call get_stack_trace for thread {}", id))
// ... into Sample.sampling_errors: Option<Vec<(Pid, anyhow::Error)>>
```

pyroscope's consumer drops each `Sample` at the end of its loop iteration
(`rust/src/pyspy_backend.rs`), which runs `drop_glue<remoteprocess::Error>`
over the copied interpreter bytes. The enum's discriminant is read from the
first word — and if that word happens to be a *small integer*, a
`String`-owning variant is selected and its "pointer" is freed:

```asm
_ZN6anyhow5error11object_drop17ha12efd9f864b56e5E:     ; ErrorImpl<remoteprocess::Error>
  +22: mov    0x48(%rbx),%rcx     ; discriminant word of remoteprocess::Error
  +26: sub    $0xa,%rcx
  +30: mov    $0x1,%eax
  +35: cmovae %rcx,%rax           ; rax = (tag >= 10) ? tag-10 : 1
  +39: cmp    $0x3,%rax
  +43: je     +79                 ; Other(String)     -> free
  +45: cmp    $0x2,%rax
  +49: je     +68                 ; IOError(io::Error)
  +51: cmp    $0x1,%rax
  +55: jne    +103                ; NoBinaryForAddress / NixError -> nothing to drop
  +57: lea    0x48(%rbx),%rdi
  +61: call   drop_in_place<goblin::error::Error>   ; -> Malformed(String) -> free
  ...
  +79: mov    0x50(%rbx),%rsi     ; "capacity"
  +88: mov    0x58(%rbx),%rdi     ; "pointer"
  +92: mov    $0x1,%edx           ; align = 1  (a String)
  +97: call   *...                ; __rust_dealloc -> free()
```

A CPython object begins with `ob_refcnt`, which is a small integer — so a
stale frame pointer that now points at a live CPython object produces
`tag = refcount`, lands in a `String` variant, and frees `ob_type` /
`ob_size` / `ob_ref` as if they were a heap pointer. That is the reporters'
`free(mem=0x18)`.

## 3. Evidence from the core dumps

Four cores, three organic and one from the deterministic probe. In every one
the dropped error's payload is a recognizable CPython object, not an error:

| core | error payload | value passed to `free()` | outcome |
|---|---|---|---|
| `artifacts/r35-w4` (wheel 1.2.1) | `{3, &PyCell_Type, 0x55d19f5c6250}` | `0x55d19f5c6250` (the cell's `ob_ref`) | `double free or corruption (out)` → SIGABRT |
| `artifacts/r6-w7` (wheel 1.2.1) | `{3, <type>, 0x564fb95e91d0}` | ditto | `double free or corruption (out)` → SIGABRT |
| `artifacts-guard/r7-w9` (guard build) | `{3, &PyList_Type, 41, ob_item}` | `0x29` = **41** = that list's `ob_size` | SIGSEGV |
| `artifacts-poc/core.9` (deterministic) | crafted | `0x29` (as planted) | SIGSEGV |

Three independent facts pin it down:

1. **The error object is intact.** With a guarding global allocator
   (`rust/src/debug_alloc.rs`, feature `debug-alloc`) the `ErrorImpl`
   allocation still carries a live magic and the correct size (0x68), and no
   invalid/double free was reported before the crash. Nothing corrupted the
   error.
2. **Its sibling field is valid.** The same `ContextError` holds
   `context: &'static str` = `{ptr, len = 0x1c = 28}` =
   `"Failed to copy PyFrameObject"`. Only the `remoteprocess::Error` half is
   CPython bytes — i.e. the `Ok` payload was *read as* an `Err`.
3. **Typed frames.** From the debug build:

```
#5  _native::debug_alloc::dealloc (ptr=0x29, layout=...)
#6  core::ptr::drop_glue<remoteprocess::Error> ()
#7  core::ptr::drop_glue<anyhow::error::ContextError<&str, remoteprocess::Error>> ()
#11 anyhow::error::object_drop<anyhow::error::ContextError<&str, remoteprocess::Error>> ()
#13 core::ptr::drop_glue<anyhow::error::ContextError<alloc::string::String, anyhow::Error>> ()
#19 core::ptr::drop_glue<(i32, anyhow::Error)> ()
#23 alloc::vec::{impl#27}::drop<(i32, anyhow::Error), alloc::alloc::Global> ()
#31 std::panicking::catch_unwind<..., _native::pyspy_backend::{impl#1}::initialize::{closure_env#0}>
```

i.e. `Vec<(Pid, anyhow::Error)>` → `ContextError<String, Error>` →
`ContextError<&str, remoteprocess::Error>` → `remoteprocess::Error` →
`free(0x29)`, inside pyroscope's py-spy consumer thread.

## 4. Why Python 3.11

`is_entry: bool` is the only `bool` in the frame struct and it exists **only**
in the 3.11 bindings; 3.12+ use `c_char` in that position:

| version | niche-bearing `bool` fields in py-spy's bindings |
|---|---|
| 3.10 and older | none in the sampled structs |
| **3.11** | **`_PyInterpreterFrame.is_entry`**, `_is._static` |
| 3.12 | `_is.{f_opcode_trace_set, sys_profile_initialized, sys_trace_initialized}` |
| 3.13 | `_is.{sys_profile_initialized, sys_trace_initialized}`, `_qsbr_thread_state.allocated`, `_stoptheworld_state.{requested, world_stopped, is_global}` |
| 3.14 | as 3.13 plus `_is.jit`, `..._.should_process/allocated` |

The frame struct is copied **for every frame of every sample**, following
`previous` pointers into memory that goes stale constantly, so 3.11 is where
this actually bites. That matches both reports (Python 3.11.14) and matches
Python 3.13 never crashing here across 3.3+ process-hours of the same
workload. On 3.12+ the remaining niches sit in interpreter-state structs that
are copied far less often and from more stable addresses.

## 5. Why it is rare

Two independent conditions have to line up in the same read:

1. **byte 68 == 2 exactly.** rustc picked the single niche value `2` for
   `Err` (`cmp $0x2,%al`), not a range, so an arbitrary garbage byte hits it
   with probability ~1/256. Values 3..255 are still UB but read back as `Ok`
   with a nonsense `bool`, which is harmless in practice.
2. **the first 8 bytes must be a small integer.** The drop only frees when
   the discriminant word selects a `String`-owning variant (`< 10`, or `13`).
   A stale frame usually starts with a *pointer*, which selects a variant with
   nothing to drop — silently harmless. It becomes fatal when the memory now
   holds a live CPython object, whose first word is a small `ob_refcnt`.

So the profiler emits bogus sampling errors routinely and only occasionally
turns one into a bad `free()`. Observed rate here: ~1 crash per 1–3
process-hours under a workload built to lose the race; the reporter sees
~12/hour across 60 workers (~1 per 5 worker-hours).

## 6. Deterministic reproducer

`poc_niche_confusion.py` plants both conditions instead of waiting for them: a
thread parks inside a Python frame, and that frame's `previous` pointer is
pointed at a private `malloc`'d buffer whose byte 68 is `2` and whose first
words decode as a `String` with pointer `0x29`. Nothing in the interpreter is
corrupted — CPython never reads the buffer, because the parked frame is never
returned from.

```
$ python poc_niche_confusion.py                 # Python 3.11 + pyroscope-io 1.2.1
parked frame wait: previous -> fake frame 0x55f40ae75c30 (is_entry=2, String ptr=0x29)
Segmentation fault                              # exit 139, within seconds, 3/3 runs

$ NO_PROFILER=1 python poc_niche_confusion.py
parked frame wait: previous -> fake frame 0x5584116b9c30 (is_entry=2, String ptr=0x29)
control run: profiler NOT started
survived                                        # exit 0
```

The core from that run has the reported stack with `free()` called on the
planted `0x29`. Because the freed value is the one written into the buffer,
the error payload can only be the `Ok` bytes reinterpreted — a genuine read
failure would have produced an `IOError` with no `String` to free.

## 7. Organic reproduction

`workload.py` + `runner.py` reproduce it without any ctypes tricks, by
maximizing how often the sampler loses the race: deep stacks pushed and popped
continuously, code objects compiled and dropped so sampled addresses go stale,
freed memory immediately reused by random bytes and pointer-filled
tuples/lists, and thread churn so OS thread ids get recycled.

Configuration that crashes:

| | |
|---|---|
| package | `pyroscope-io==1.2.1` (PyPI wheel, unmodified) |
| python | 3.11 (`python:3.11-slim`) |
| profiler | CPU only (`mem_enabled=False`) |
| config | `sample_rate=997`, `oncpu=False`, `gil_only=False`, `report_pid/thread_id/thread_name=True` |
| env | `PYTHONMALLOC=malloc` |
| load | 8–10 worker processes, 60 s each |
| rate | ~1 crash per 1–3 process-hours |

`PYTHONMALLOC=malloc` is an amplifier, not the cause: it puts CPython's
objects in the same glibc heap the sampler reads, so memory freed by CPython
is recycled immediately and a stale frame read is much more likely to land on
a *live* object with a small refcount (condition 2 above). Runs with the
default allocator did not crash in 4.2 process-hours — consistent with the
reporter's much lower per-worker rate rather than with a different bug.

Configurations that did **not** crash (all with the same workload):

* Python 3.13, default allocator, 200 runs × 60 s (~3.3 process-hours).
* Python 3.11 under valgrind/memcheck (memcheck serializes threads, which
  suppresses the race).
* 16 procs × 16 threads × depth 150 × 2000 Hz (~1.4 process-hours) — more
  load is not the axis that matters.
* `FRAME_CYCLE=1`, which makes py-spy emit a sampling error on *every* sample
  (~0.9 process-hours) — error volume alone is not the axis either.

## 8. Fix directions

1. **Remove the niche** (smallest, most targeted): generate `u8`/`c_char`
   instead of `bool` for these fields in py-spy's bindings.
   `Result<T, remoteprocess::Error>` then gets a real tag and the confusion
   becomes impossible. `is_entry` is only used as a truthy flag
   (`frame.is_entry()`), so the change is mechanical.
2. **Tighten `copy_struct`'s bound** so only types valid for every bit pattern
   can be read out of another process — e.g. `T: bytemuck::AnyBitPattern`
   instead of `T: Copy`. This is the real fix: the current signature makes the
   bug trivial to reintroduce with any future binding that has a `bool`,
   `char`, enum, reference or `NonNull` field.
3. Optionally, treat "byte out of range" as a sampling error explicitly rather
   than letting the niche decide.

Both live upstream (py-spy / remoteprocess), so pyroscope needs a patched pin
in `rust/Cargo.toml`.

## 9. Tooling built for this investigation

| artifact | what it is |
|---|---|
| `Dockerfile` | Python 3.11 + `pyroscope-io==1.2.1` + gdb; the reproducing image |
| `Dockerfile.debug` | release build of `rust/` with full debug info (`CARGO_PROFILE_RELEASE_DEBUG=2`, `CARGO_PROFILE_RELEASE_STRIP=none`) — gives typed frames in cores |
| `Dockerfile.guard` + `rust/src/debug_alloc.rs` | guarding global allocator (feature `debug-alloc`): magic + size header per allocation, aborts with a backtrace on invalid/double free, size mismatch, or a trashed neighbour. Proved the error object was *not* corrupted |
| `Dockerfile.asan` / `run-asan.sh` | `-Zsanitizer=address` build, gcc `libasan` preloaded (rustc ships only the static runtime) |
| `Dockerfile.valgrind` / `run-valgrind.sh` | same workload under memcheck |
| `analyze-core.sh` | batch `gdb` backtraces for every core under `artifacts/` |
| `poc_niche_confusion.py` | the deterministic reproducer (§6) |
| `poc_frame_cycle.py` | makes py-spy's frame walk hit its 4096-frame bound every sample |
| `poc_hostile_string.py`, `poc_ucs4_garbage.py` | probes for py-spy's UCS-4 `copy_string` path |
| `poc_shutdown_race.py` | configure/shutdown cycles under load |
| `forkload.py`, `tls_sink.py` | fork-shaped variant and an HTTPS ingest sink |

## 10. Hypotheses ruled out along the way

* **ABI mismatch** (the issue's original theory). The extension links no
  Python symbols; 1.2.1 ships a per-version `.so` and still crashes.
* **Memory profiling.** Disabled throughout; the crash predates the feature.
* **py-spy's invalid-`char` UB in `copy_string`** (`from_raw_parts(... as
  *const char)` over bytes read from the target). Real UB and worth fixing —
  it produces `String`s holding invalid UTF-8 — but `poc_hostile_string.py`
  feeds it non-scalar values on *every* sample without crashing.
* **A double-drop in `std::sync::mpsc`.** 83 million messages across 2487
  receiver-drop races under ASAN, all accounted for, no reports.
* **Unbounded frame walk.** `get_stack_trace` does bound `frames` at 4096.
* **Heap corruption from pyroscope's own allocations.** The guard allocator
  saw no invalid free, no double free, no size mismatch and no trashed
  neighbours before the crash.
* **The fork paths / `os.register_at_fork` handlers, and the HTTPS upload path
  (native-tls/OpenSSL).** Exercised by `forkload.py` and `TLS=1` without
  reproducing; the crashing runs use neither.
