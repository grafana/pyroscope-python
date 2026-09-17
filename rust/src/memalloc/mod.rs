//! The memory (heap allocation) profiler.
//!
//! # The one rule: PyMem versus libc malloc
//!
//! Everything reachable from an allocator hook runs *inside* CPython's
//! allocator, with the GIL held. Two very different kinds of allocation can
//! happen there and only one of them is survivable:
//!
//! * **Fatal -- allocates via PyMem, re-enters our hooks.** Never call, from
//!   anywhere in this module tree: `PyObject_Malloc` / `PyMem_Malloc`;
//!   `Py_INCREF` / `Py_DECREF` (a decref can free, which re-enters the `free`
//!   hook); `PyUnicode_AsUTF8AndSize` (it caches a UTF-8 buffer through
//!   `PyObject_Malloc`, which is why the frame walker only decodes
//!   compact-ASCII strings and reports `<non-ascii>` otherwise);
//!   `PyCode_Addr2Line`; `PyThreadState_GetFrame` / `PyFrame_GetBack` /
//!   `PyFrame_GetCode` (all return new references); `PyErr_*`;
//!   `PyGILState_Ensure`; any PyO3 `Bound` / `Py<T>` / `Python::attach`.
//!
//! * **Acceptable -- allocates via libc `malloc`.** Rust's global allocator is
//!   `System`, so `Vec`, `HashMap` and `Box` growth cannot re-enter PyMem.
//!   This is already how the C++ profiler behaves. It follows that this crate
//!   must never define a `#[global_allocator]`: routing Rust's allocator to
//!   PyMem would make the hook path infinitely recursive.
//!
//! # No panics
//!
//! The hooks are `extern "C"` function pointers called by CPython. Since Rust
//! 1.81 a panic escaping such a function is not undefined behaviour, but it
//! does `abort()` -- so a profiler bug becomes a hard crash of the host
//! process. `catch_unwind` is not the answer either: it runs the panic hook,
//! which is arbitrary user code and symbolization that takes loader locks.
//!
//! Instead this module tree is panic-free by construction, enforced by the
//! `deny` list below. Note that these are `restriction`-group clippy lints:
//! they are allowed by default and are *not* switched on by `--deny warnings`,
//! so the attribute is what does the work. They are also syntactic and
//! per-item, meaning they do not follow calls into other modules -- which is
//! precisely why all hook-reachable code has to live under `memalloc/`.
//!
//! Any `#[allow]` of a panic lint in here must carry a `PANIC-OK:` comment
//! explaining why the operation cannot panic.
#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::get_unwrap,
    clippy::unwrap_in_result,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::unreachable,
    clippy::todo,
    clippy::unimplemented,
    clippy::string_slice,
    clippy::exit,
    clippy::arithmetic_side_effects,
    clippy::integer_division,
    clippy::modulo_arithmetic,
    clippy::cast_possible_truncation,
    unconditional_panic,
    arithmetic_overflow,
    unused_must_use
)]

// The allocator hook relies on the GIL serialising it, and the frame walker
// reads structs whose layout differs under free-threading. setup.py already
// declines to enable the feature on such a build; this makes it impossible to
// get wrong rather than merely unlikely.
#[cfg(all(feature = "memory", Py_GIL_DISABLED))]
compile_error!(
    "the `memory` feature does not support free-threaded CPython: the allocator hook relies on the GIL"
);

pub mod limits;
pub mod pure;
pub mod reentrancy;
pub mod sink;

#[cfg(feature = "memory")]
pub mod runtime;

#[cfg(test)]
pub mod testing;

#[cfg(test)]
pub mod tests;
