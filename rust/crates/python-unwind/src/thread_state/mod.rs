//! Finds the calling thread's `PyThreadState` from a signal handler, like
//! `PyGILState_GetThisThreadState` but without the dynamic loader's TLS access path.
//!
//! [`current`] has one implementation per platform:
//! - `pre_3_15`: up to 3.14, the API, on every platform;
//! - [`glibc`]: 3.15 on glibc (x86-64 and aarch64), direct reads at the location that
//!   `pyroscope_python_thread_state::init` found, verified and stored with `set_location`;
//! - `musl`: 3.15 on musl, the API;
//! - [`macos`]: 3.15 on macOS (x86-64 and arm64), a load at the location in the block of
//!   CPython's pthread key that `pyroscope_python_thread_state::init` found, verified and
//!   stored with `set_location`;
//! - `unsupported`: 3.15 elsewhere, which finds no thread state.

// Compiled for every CPython version, so that its tests and the search in
// `pyroscope_python_thread_state` run on all of them.
#[cfg(all(
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod glibc;
#[cfg(all(
    Py_3_15,
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use glibc::{current, location, set_location};

#[cfg(not(Py_3_15))]
mod pre_3_15;
#[cfg(not(Py_3_15))]
pub use pre_3_15::current;

#[cfg(all(Py_3_15, target_os = "linux", target_env = "musl"))]
mod musl;
#[cfg(all(Py_3_15, target_os = "linux", target_env = "musl"))]
pub use musl::current;

// Compiled for every CPython version, so that its tests and the search in
// `pyroscope_python_thread_state` run on all of them.
#[cfg(all(
    target_os = "macos",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub mod macos;
#[cfg(all(
    Py_3_15,
    target_os = "macos",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use macos::{current, location, set_location};

#[cfg(all(
    Py_3_15,
    not(all(target_os = "linux", target_env = "musl")),
    not(all(
        target_os = "linux",
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )),
    not(all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
))]
mod unsupported;
#[cfg(all(
    Py_3_15,
    not(all(target_os = "linux", target_env = "musl")),
    not(all(
        target_os = "linux",
        target_env = "gnu",
        any(target_arch = "x86_64", target_arch = "aarch64")
    )),
    not(all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))
))]
pub use unsupported::current;
