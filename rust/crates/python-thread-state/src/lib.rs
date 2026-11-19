//! Prepares `pyroscope_python_unwind::thread_state::current` to find the calling thread's
//! `PyThreadState` from a signal handler, like `PyGILState_GetThisThreadState` but without the
//! dynamic loader's TLS access path.
//!
//! Call [`init`] once on a normal thread attached to the interpreter. Nothing in this crate is
//! async-signal-safe: the signal-time reads live in `pyroscope_python_unwind`.
//!
//! CPython 3.15 keeps the thread state in a thread-local variable, and reaching it through
//! `PyGILState_GetThisThreadState` can allocate and lock inside glibc's dynamic loader. On
//! glibc (x86-64 and aarch64) the handler reads the variable directly: at a fixed offset from
//! the thread pointer, or through the thread's DTV when libpython was `dlopen`ed into dynamic
//! TLS. [`init`] picks one and verifies it against the loader on two threads, because both
//! rely on glibc's private TCB and DTV layouts. musl needs none of this: its TLS accesses are
//! plain loads. Other platforms are unsupported on 3.15.
//!
//! The search uses kindasafe: recovery must be installed and its signals unblocked on the
//! calling thread.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadStateError {
    /// No async-signal-safe way to find the thread state is implemented for this platform.
    Unsupported,
    /// The module defining `PyGILState_GetThisThreadState` has no TLS segment.
    NoTlsSegment,
    /// The TLS block of CPython does not hold the thread state in exactly one slot.
    VariableNotFound,
    /// The variable's location did not hold on two threads.
    UnknownTlsLayout,
}

// `init` has one implementation per platform, matching
// `pyroscope_python_unwind::thread_state::current`.

// Compiled for every CPython version, so that the search is tested on all of them.
#[cfg(all(
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[cfg_attr(not(Py_3_15), allow(dead_code))]
mod glibc;
#[cfg(all(
    Py_3_15,
    target_os = "linux",
    target_env = "gnu",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use glibc::init;

#[cfg(not(Py_3_15))]
mod pre_3_15;
#[cfg(not(Py_3_15))]
pub use pre_3_15::init;

#[cfg(all(Py_3_15, target_os = "linux", target_env = "musl"))]
mod musl;
#[cfg(all(Py_3_15, target_os = "linux", target_env = "musl"))]
pub use musl::init;

#[cfg(all(
    Py_3_15,
    not(all(target_os = "linux", target_env = "musl")),
    not(all(
        target_os = "linux",
        target_env = "gnu",
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
    ))
))]
pub use unsupported::init;
