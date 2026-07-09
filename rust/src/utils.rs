use crate::error::Result;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

#[derive(Debug, PartialEq, Clone)]
pub struct TimeRange {
    from_unix: Duration,
    duration: Duration,
}

impl TimeRange {
    pub fn new(from: SystemTime, until: SystemTime) -> Result<TimeRange> {
        let from_unix = from.duration_since(UNIX_EPOCH)?;
        let duration = until.duration_since(from).unwrap_or(Duration::ZERO);
        Ok(Self {
            from_unix,
            duration,
        })
    }

    pub fn start_time_unix(&self) -> Duration {
        self.from_unix
    }
    pub fn duration(&self) -> Duration {
        self.duration
    }
}
