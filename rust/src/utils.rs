pub use pyroscope_pprof::TimeRange;
use std::fmt;

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub struct ThreadId {
    pthread: libc::pthread_t,
}

// SAFETY: pthread_t is an opaque thread identifier used as a handle,
// never dereferenced. On musl it's *mut c_void, on glibc it's c_ulong.
unsafe impl Send for ThreadId {}
unsafe impl Sync for ThreadId {}

impl From<libc::pthread_t> for ThreadId {
    fn from(value: libc::pthread_t) -> Self {
        Self { pthread: value }
    }
}
impl ThreadId {
    pub fn pthread_self() -> Self {
        Self {
            pthread: unsafe { libc::pthread_self() },
        }
    }
}

impl fmt::Display for ThreadId {
    #[cfg(target_env = "musl")]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", { self.pthread as libc::uintptr_t })
    }
    #[cfg(not(target_env = "musl"))]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", { self.pthread })
    }
}
