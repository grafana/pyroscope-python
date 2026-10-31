use std::sync::{Mutex, MutexGuard, Once};
use std::time::Duration;

/// Tests in one binary share the signal disposition and counters, so they take this lock.
/// SIGPROF starts ignored so a signal that lands after a handler is dropped cannot kill the test.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    static IGNORE: Once = Once::new();
    IGNORE.call_once(|| unsafe {
        libc::signal(libc::SIGPROF, libc::SIG_IGN);
    });
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn thread_cpu_time() -> Duration {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

pub fn burn_cpu(amount: Duration) {
    let end = thread_cpu_time() + amount;
    let mut x = 0u64;
    while thread_cpu_time() < end {
        for i in 0..1000 {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(i));
        }
    }
}
