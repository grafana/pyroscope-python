//! Signal-safe producer notification. See `signal-notification.md` for the protocol proof.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[cfg(not(all(target_has_atomic = "ptr", target_has_atomic = "8")))]
compile_error!("signal notification requires lock-free pointer and byte atomics");

pub(super) static REGISTRY: Registry = Registry::new();
pub(super) const MAX_DELAY: Duration = Duration::from_millis(100);

pub(super) struct Registry {
    pointer: AtomicPtr<Notification>,
    readers: AtomicUsize,
}

impl Registry {
    const fn new() -> Self {
        Self {
            pointer: AtomicPtr::new(ptr::null_mut()),
            readers: AtomicUsize::new(0),
        }
    }

    // Only called while the session owns the exclusive signal-handler guard.
    pub(super) fn register(&self, notification: Arc<Notification>) -> Registration<'_> {
        self.pointer
            .store(Arc::as_ptr(&notification).cast_mut(), Ordering::SeqCst);
        Registration {
            registry: self,
            _notification: notification,
        }
    }

    pub(super) fn with(&self, f: impl FnOnce(&Notification)) {
        self.readers.fetch_add(1, Ordering::SeqCst);
        let pointer = self.pointer.load(Ordering::SeqCst);
        if !pointer.is_null() {
            // SAFETY: unregister clears the pointer and waits for all readers before freeing it.
            f(unsafe { &*pointer });
        }
        self.readers.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct Registration<'a> {
    registry: &'a Registry,
    _notification: Arc<Notification>,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.registry
            .pointer
            .store(ptr::null_mut(), Ordering::SeqCst);
        while self.registry.readers.load(Ordering::SeqCst) != 0 {
            std::thread::yield_now();
        }
    }
}

pub(crate) struct Notification {
    read: OwnedFd,
    write: OwnedFd,
    // Zero while draining; otherwise the occupancy that should wake the consumer.
    threshold: AtomicUsize,
}

impl Notification {
    pub(super) fn new() -> io::Result<Self> {
        let mut fds = [-1; 2];
        // All descriptor creation/configuration happens before publication to the handler.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        for fd in [&read, &write] {
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1
                || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1
            {
                return Err(io::Error::last_os_error());
            }
        }
        let notification = Self {
            read,
            write,
            threshold: AtomicUsize::new(0),
        };
        // Resolve the native write/errno entry points before callbacks can use them.
        notification.wake();
        notification.clear()?;
        Ok(notification)
    }

    /// Called after publication, even when the consumer is already active. The RMW
    /// hands publication to the consumer's arm operation, preventing a lost idle wakeup.
    pub(super) fn published(&self, occupied: impl FnOnce() -> usize) {
        let threshold = self.threshold.fetch_or(0, Ordering::AcqRel);
        if threshold != 0
            && occupied() >= threshold
            && self
                .threshold
                .compare_exchange(threshold, 0, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            self.wake();
        }
    }

    /// Also used by shutdown; it must bypass threshold coalescing.
    pub(crate) fn wake(&self) {
        unsafe {
            let errno = errno_pointer();
            let saved = *errno;
            let byte = 1u8;
            loop {
                let result = libc::write(self.write.as_raw_fd(), (&byte as *const u8).cast(), 1);
                if result == 1 || *errno != libc::EINTR {
                    // EAGAIN means the pipe is already readable. Other errors cannot
                    // occur for this owned pipe with a live reader and a valid byte.
                    break;
                }
            }
            *errno = saved;
        }
    }

    // Only the consumer reads the pipe. Called before arming, never after the recheck.
    fn clear(&self) -> io::Result<()> {
        let mut bytes = [0u8; 128];
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
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EAGAIN) => return Ok(()),
                _ => return Err(error),
            }
        }
    }

    pub(super) fn wait(
        &self,
        threshold: usize,
        timeout: Option<Duration>,
        running: &AtomicBool,
        occupied: impl FnOnce() -> usize,
    ) -> io::Result<()> {
        self.clear()?;
        self.threshold.swap(threshold, Ordering::AcqRel);
        let result = if occupied() >= threshold || !running.load(Ordering::Acquire) {
            Ok(())
        } else {
            self.poll(timeout)
        };
        self.threshold.swap(0, Ordering::AcqRel);
        result
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<()> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            let millis = deadline.map_or(-1, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .saturating_add(1)
                    .min(i32::MAX as u128) as i32
            });
            let mut fd = libc::pollfd {
                fd: self.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut fd, 1, millis) };
            if result >= 0 {
                return if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::other("notification pipe failed"))
                };
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(());
            }
        }
    }
}

// These are libc's existing per-thread errno slots, not Rust TLS or lazy initialization.
#[cfg(target_os = "linux")]
unsafe fn errno_pointer() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}
#[cfg(target_os = "macos")]
unsafe fn errno_pointer() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

#[cfg(all(test, not(miri)))]
mod tests;
