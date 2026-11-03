//! Initializes kindasafe recovery once. SDK users must ensure that no code changes
//! the SIGSEGV/SIGBUS handlers after initialization; we do not check their ownership.

#[derive(Debug, Clone)]
pub enum InitError {
    Install(kindasafe_init::InitError),
}

/// Installs recovery once; kindasafe caches the result, including failures.
/// SDK users must prevent concurrent SIGSEGV/SIGBUS changes during installation and
/// keep the installed handlers unchanged afterward, including across profiler restarts.
/// Previously installed handlers must support chaining and remain loaded.
pub fn init() -> Result<(), InitError> {
    // SAFETY: kindasafe serializes its installation. SDK users must coordinate other
    // signal users and keep chained callbacks loaded as documented above and in the
    // crate-level contract.
    unsafe { kindasafe_init::init() }.map_err(InitError::Install)
}
