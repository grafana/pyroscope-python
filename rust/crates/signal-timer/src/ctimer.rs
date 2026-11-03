use libc::c_int;
use std::io;
use std::mem;
use std::time::Duration;

const SIGEV_THREAD_ID: c_int = 4;

/// `MAKE_THREAD_CPUCLOCK(tid, CPUCLOCK_SCHED)` from the kernel; libc only accepts its own clocks.
fn thread_cpu_clock(tid: libc::pid_t) -> libc::clockid_t {
    ((!(tid as u32) << 3) | 6) as libc::clockid_t
}

/// Timer on the CPU clock of one thread that delivers `signal` to that thread. Deleted on drop.
pub struct CTimer {
    id: c_int,
}

impl CTimer {
    pub fn start(tid: libc::pid_t, signal: c_int, interval: Duration) -> io::Result<Self> {
        let mut sev: libc::sigevent = unsafe { mem::zeroed() };
        sev.sigev_notify = SIGEV_THREAD_ID;
        sev.sigev_signo = signal;
        sev.sigev_notify_thread_id = tid;
        let mut id: c_int = 0;
        let clock = thread_cpu_clock(tid);
        if unsafe { libc::syscall(libc::SYS_timer_create, clock, &sev, &mut id) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let timer = CTimer { id };
        let ts = libc::timespec {
            tv_sec: interval.as_secs() as _,
            tv_nsec: interval.subsec_nanos() as _,
        };
        let spec = libc::itimerspec {
            it_interval: ts,
            it_value: ts,
        };
        let null = std::ptr::null_mut::<libc::itimerspec>();
        if unsafe { libc::syscall(libc::SYS_timer_settime, id, 0, &spec, null) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(timer)
    }
}

impl Drop for CTimer {
    fn drop(&mut self) {
        unsafe { libc::syscall(libc::SYS_timer_delete, self.id) };
    }
}
