//! 3.15 keeps the thread state in a thread-local variable, and reading it through the API is
//! not known to be async-signal-safe on this platform.

/// Always 0: no thread state can be found on this platform.
#[inline(always)]
pub fn current() -> usize {
    0
}
