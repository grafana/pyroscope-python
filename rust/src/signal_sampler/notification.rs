//! Native wakeup for the signal producer and the sampling thread.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

#[cfg(not(all(target_has_atomic = "32", target_has_atomic = "ptr")))]
compile_error!("signal notification requires lock-free 32-bit and pointer atomics");
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("signal notification is validated only on x86_64 and aarch64");

static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static CALLBACKS: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct Notification {
    read: OwnedFd,
    #[cfg(target_os = "macos")]
    write: OwnedFd,
}

impl Notification {
    pub fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                read: unsafe { OwnedFd::from_raw_fd(fd) },
            })
        }
        #[cfg(target_os = "macos")]
        {
            let mut fds = [-1; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
            let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
            for fd in [&read, &write] {
                if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
                if flags < 0
                    || unsafe {
                        libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                    } < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(Self { read, write })
        }
    }

    fn write_fd(&self) -> RawFd {
        #[cfg(target_os = "linux")]
        {
            self.read.as_raw_fd()
        }
        #[cfg(target_os = "macos")]
        {
            self.write.as_raw_fd()
        }
    }

    pub fn activate(&self) {
        WRITE_FD.store(self.write_fd(), Ordering::SeqCst);
    }

    pub fn deactivate(&self) {
        WRITE_FD.store(-1, Ordering::SeqCst);
        while CALLBACKS.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }

    pub fn wake(&self) {
        write_fd(self.write_fd());
    }

    pub fn wait(&self) -> io::Result<()> {
        let mut pfd = libc::pollfd {
            fd: self.read.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let result = unsafe { libc::poll(&mut pfd, 1, -1) };
            if result > 0 {
                break;
            }
            if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(target_os = "linux")]
        let mut bytes = [0u8; 8];
        #[cfg(target_os = "macos")]
        let mut bytes = [0u8; 256];
        loop {
            let n = unsafe {
                libc::read(
                    self.read.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if n > 0 {
                continue;
            }
            if n == 0 {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            return Err(error);
        }
    }
}

// Called only from the signal callback. The count keeps a descriptor open until every
// callback that could have loaded it has finished its write.
pub(super) fn enter_callback() -> Option<RawFd> {
    CALLBACKS.fetch_add(1, Ordering::SeqCst);
    let fd = WRITE_FD.load(Ordering::SeqCst);
    if fd < 0 {
        CALLBACKS.fetch_sub(1, Ordering::SeqCst);
        None
    } else {
        Some(fd)
    }
}

pub(super) fn leave_callback(fd: RawFd, wake: bool) {
    if wake {
        write_fd(fd);
    }
    CALLBACKS.fetch_sub(1, Ordering::SeqCst);
}

fn write_fd(fd: RawFd) {
    #[cfg(target_os = "linux")]
    let value = 1u64;
    #[cfg(target_os = "linux")]
    let (ptr, len) = ((&raw const value).cast(), 8);
    #[cfg(target_os = "macos")]
    let value = 1u8;
    #[cfg(target_os = "macos")]
    let (ptr, len) = ((&raw const value).cast(), 1);
    // write is async-signal-safe. A full nonblocking descriptor already has
    // a pending wake. Retry interruption, which might not have written one.
    loop {
        let result = unsafe { libc::write(fd, ptr, len) };
        if result >= 0 {
            return;
        }
        #[cfg(target_os = "linux")]
        let errno = unsafe { *libc::__errno_location() };
        #[cfg(target_os = "macos")]
        let errno = unsafe { *libc::__error() };
        if errno != libc::EINTR {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn coalesced_wakes_and_restart_release_descriptors() {
        let _guard = TEST_LOCK.lock().unwrap();
        for _ in 0..3 {
            let notification = std::sync::Arc::new(Notification::new().unwrap());
            let fd = notification.read.as_raw_fd();
            let (tx, rx) = mpsc::channel();
            let reader = notification.clone();
            let thread = std::thread::spawn(move || {
                reader.wait().unwrap();
                tx.send(()).unwrap();
            });
            for _ in 0..100 {
                notification.wake();
            }
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
            thread.join().unwrap();
            notification.deactivate();
            drop(notification);
            assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        }
    }

    #[test]
    fn shutdown_waits_for_a_callback_before_releasing_its_descriptor() {
        let _guard = TEST_LOCK.lock().unwrap();
        let notification = std::sync::Arc::new(Notification::new().unwrap());
        notification.activate();
        let fd = enter_callback().unwrap();
        let (done, completed) = mpsc::channel();
        let owner = notification.clone();
        let thread = std::thread::spawn(move || {
            owner.deactivate();
            done.send(()).unwrap();
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while WRITE_FD.load(Ordering::SeqCst) != -1 {
            assert!(
                std::time::Instant::now() < deadline,
                "deactivation did not start"
            );
            std::thread::yield_now();
        }
        assert!(completed.recv_timeout(Duration::from_millis(20)).is_err());
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
        leave_callback(fd, true);
        completed.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
        assert!(enter_callback().is_none());
    }
}
