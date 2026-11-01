//! OS-mediated snapshots for the deferred resolver, never called by a signal handler.
//!
//! Recorded addresses do not keep CPython allocations alive. The kernel copies bytes into
//! owned storage without us dereferencing a stale pointer or constructing a source reference.
//! A successful copy is not a coherent object snapshot: the resolver still bounds and validates
//! its input, and an address reused for another object can produce a misattributed sample.

use std::io;

pub struct Memory {
    #[cfg(target_os = "linux")]
    file: std::fs::File,
}

impl Memory {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            // Open for each session, so a session started after fork reads the child's mm.
            #[cfg(target_os = "linux")]
            file: std::fs::File::open("/proc/self/mem")?,
        })
    }

    /// An error may leave a partial copy in `out`; the caller must discard it.
    pub fn read(&self, address: usize, out: &mut [u8]) -> io::Result<()> {
        address.checked_add(out.len()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "memory address range overflow")
        })?;
        if out.is_empty() {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::FileExt;
            self.file.read_exact_at(out, address as u64)
        }
        #[cfg(target_os = "macos")]
        {
            let mut copied = 0;
            // SAFETY: the source is a numeric address for the kernel to validate. The
            // destination is exclusively borrowed, initialized storage of the stated length;
            // copied is a valid out-parameter. No pointer/reference to the source is created.
            let status = unsafe {
                mach2::vm::mach_vm_read_overwrite(
                    mach2::traps::mach_task_self(),
                    address as u64,
                    out.len() as u64,
                    out.as_mut_ptr() as u64,
                    &mut copied,
                )
            };
            if status != mach2::kern_return::KERN_SUCCESS {
                return Err(io::Error::other(format!(
                    "mach_vm_read_overwrite: {status}"
                )));
            }
            if copied != out.len() as u64 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "short VM copy",
                ));
            }
            Ok(())
        }
    }
}

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;

    #[test]
    fn snapshots_owned_bytes_and_rejects_invalid_addresses() {
        let memory = Memory::new().unwrap();
        let source = [11, 22, 33, 44];
        let mut out = [0; 4];
        memory.read(source.as_ptr() as usize, &mut out).unwrap();
        assert_eq!(out, source);
        assert!(memory.read(0, &mut out).is_err());
        assert!(memory.read(usize::MAX - 1, &mut out).is_err());
        memory.read(0, &mut []).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_short_copies() {
        // Exercise the same read_exact_at boundary without depending on the VM layout.
        let memory = Memory {
            file: std::fs::File::open("/dev/null").unwrap(),
        };
        assert_eq!(
            memory.read(0, &mut [0; 8]).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
