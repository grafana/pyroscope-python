//! Logic with no dependency on a live interpreter.
//!
//! These modules are compiled in every configuration, including
//! `--no-default-features`, so that `cargo miri test --lib
//! --no-default-features` covers them. Nothing here may reference `pyo3` or
//! any libpython symbol.

pub mod frames;
pub mod linetable;
pub mod offsets;
pub mod rng;
pub mod sampler;
