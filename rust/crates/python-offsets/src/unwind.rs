//! Field offsets of the CPython this crate is compiled against, default (GIL) build, one file per
//! minor version. `build.rs` checks the selected one against the headers, see `python_offsets.c`.
//!
//! - `THREAD_FRAME`: offset in `PyThreadState` of the current frame, or of `cframe` if
//!   `CFRAME_FRAME` is present (3.11–3.12).
//! - `CFRAME_FRAME`: offset in `_PyCFrame` of the current frame, only on 3.11–3.12.
//! - `OWNER` and `FRAME_OWNED_BY_CSTACK` are emitted from 3.12.
//! - `FRAME_OWNED_BY_INTERPRETER` and `EXECUTABLE_TAG_BITS` are emitted from 3.14.

#[cfg(all(Py_3_11, not(Py_3_12)))]
mod v3_11;
#[cfg(all(Py_3_11, not(Py_3_12)))]
pub use v3_11::*;

#[cfg(all(Py_3_12, not(Py_3_13)))]
mod v3_12;
#[cfg(all(Py_3_12, not(Py_3_13)))]
pub use v3_12::*;

#[cfg(all(Py_3_13, not(Py_3_14)))]
mod v3_13;
#[cfg(all(Py_3_13, not(Py_3_14)))]
pub use v3_13::*;

#[cfg(all(Py_3_14, not(Py_3_15)))]
mod v3_14;
#[cfg(all(Py_3_14, not(Py_3_15)))]
pub use v3_14::*;
