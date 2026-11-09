//! Experimental CPU profiling of the signal profiler's own worker threads.

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod linux;
#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use linux::{Session, register, sample};

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
mod unsupported {
    pub struct Session;
    impl Session {
        pub fn start(_: u32) -> Result<Self, String> {
            Ok(Self)
        }
    }
    pub fn register(_: usize) {}
    pub unsafe fn sample(_: *mut libc::siginfo_t, _: *mut libc::c_void) -> bool {
        false
    }
}
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
pub use unsupported::{Session, register, sample};

pub const READER: usize = 0;
pub const REPORTER: usize = 1;
pub const UPLOADER: usize = 2;
