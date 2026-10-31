use std::io;
use std::time::Duration;

/// Process-wide `ITIMER_PROF` timer delivering `SIGPROF`. Disarmed on drop.
pub struct ITimer {
    _private: (),
}

impl ITimer {
    pub fn start(interval: Duration) -> io::Result<Self> {
        let micros = interval.as_micros().max(1);
        set_itimer(libc::timeval {
            tv_sec: (micros / 1_000_000) as _,
            tv_usec: (micros % 1_000_000) as _,
        })?;
        Ok(ITimer { _private: () })
    }
}

impl Drop for ITimer {
    fn drop(&mut self) {
        let _ = set_itimer(libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        });
    }
}

fn set_itimer(tv: libc::timeval) -> io::Result<()> {
    let value = libc::itimerval {
        it_interval: tv,
        it_value: tv,
    };
    if unsafe { libc::setitimer(libc::ITIMER_PROF, &value, std::ptr::null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
