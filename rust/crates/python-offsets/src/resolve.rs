//! Offsets and `PyASCIIObject.state` bits of the CPython this crate is compiled against, default
//! (GIL) build, one file per minor version. `build.rs` checks the selected one against the
//! headers, see `python_offsets.c`.
//!
//! - `ASCII_SIZE`, `COMPACT_SIZE`: where the characters of a compact ascii and a compact
//!   non-ascii string start.
//! - `STATE_*`: masks of the `state` bit fields in the word at `UNICODE_STATE`.

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

#[cfg(all(Py_3_15, not(Py_3_16)))]
mod v3_15;
#[cfg(all(Py_3_15, not(Py_3_16)))]
pub use v3_15::*;
