//! The parts that need a live CPython.
//!
//! Everything here is behind `#[cfg(feature = "memory")]`. The logic in
//! [`super::pure`] is deliberately kept out so that
//! `cargo miri test --lib --no-default-features` covers it.

pub mod pyapi;
