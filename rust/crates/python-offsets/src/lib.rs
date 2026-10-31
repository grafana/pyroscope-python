//! Checked CPython layouts for the signal sampler, selected by pyo3's Python version cfgs.
//! `build.rs` compares the selected files with the target interpreter's headers. To regenerate
//! them, build with `PYROSCOPE_UPDATE_PYTHON_LAYOUTS=1` and `Python3_EXECUTABLE` / `PYO3_PYTHON`
//! pointing to that interpreter. Python 3.10 builds expose no layouts, allowing the SDK
//! to reject signal sampling at runtime.

#[cfg(any(not(Py_3_10), Py_3_15))]
compile_error!("the SDK builds on CPython 3.10 to 3.14; signal sampling requires 3.11 to 3.14");
#[cfg(not(target_pointer_width = "64"))]
compile_error!("the signal sampler requires a 64-bit target");
#[cfg(any(PyPy, GraalPy, Py_GIL_DISABLED, Py_LIMITED_API))]
compile_error!("the signal sampler requires a default GIL CPython build with the full C API");

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
