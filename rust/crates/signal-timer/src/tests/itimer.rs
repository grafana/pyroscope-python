use super::common::{burn_cpu, serial};
use crate::{Mode, SignalHandler, Timer, c_int, siginfo_t};
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_signal(_: c_int, _: *mut siginfo_t, _: *mut c_void) {
    COUNT.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn fires_while_burning_cpu_and_stops_on_drop() {
    let _serial = serial();
    let _handler = SignalHandler::install(Timer::SIGNAL, on_signal).unwrap();
    COUNT.store(0, Ordering::Relaxed);

    let timer = Timer::start(Mode::ITimer, Duration::from_millis(1)).unwrap();
    burn_cpu(Duration::from_millis(200));
    drop(timer);
    let fired = COUNT.load(Ordering::Relaxed);
    assert!(fired >= 100, "fired {fired} times");

    burn_cpu(Duration::from_millis(50));
    assert_eq!(COUNT.load(Ordering::Relaxed), fired);
}

#[test]
fn does_not_fire_while_idle() {
    let _serial = serial();
    let _handler = SignalHandler::install(Timer::SIGNAL, on_signal).unwrap();
    COUNT.store(0, Ordering::Relaxed);

    let _timer = Timer::start(Mode::ITimer, Duration::from_millis(1)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let fired = COUNT.load(Ordering::Relaxed);
    assert!(fired < 10, "fired {fired} times");
}

#[test]
fn sub_microsecond_interval_rounds_up() {
    let _serial = serial();
    let _handler = SignalHandler::install(Timer::SIGNAL, on_signal).unwrap();
    COUNT.store(0, Ordering::Relaxed);

    let _timer = Timer::start(Mode::ITimer, Duration::from_nanos(1)).unwrap();
    burn_cpu(Duration::from_millis(20));
    assert!(COUNT.load(Ordering::Relaxed) > 0);
}

#[test]
fn only_one_timer_at_a_time() {
    let _serial = serial();
    let timer = Timer::start(Mode::ITimer, Duration::from_millis(10)).unwrap();
    let err = Timer::start(Mode::ITimer, Duration::from_millis(10))
        .err()
        .unwrap();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    #[cfg(target_os = "linux")]
    {
        let err = Timer::start(Mode::CTimer, Duration::from_millis(10))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }
    drop(timer);
    Timer::start(Mode::ITimer, Duration::from_millis(10)).unwrap();
}

#[test]
fn thread_events_do_nothing() {
    let _serial = serial();
    let _handler = SignalHandler::install(Timer::SIGNAL, on_signal).unwrap();
    COUNT.store(0, Ordering::Relaxed);

    let timer = Timer::start(Mode::ITimer, Duration::from_millis(1)).unwrap();
    assert_eq!(timer.mode(), Mode::ITimer);
    timer.thread_created(0x0fff_ffff).unwrap();
    timer.thread_removed(0x0fff_ffff);
    burn_cpu(Duration::from_millis(50));
    assert!(COUNT.load(Ordering::Relaxed) >= 20);
}

#[test]
fn zero_interval_is_rejected() {
    let _serial = serial();
    let err = Timer::start(Mode::ITimer, Duration::ZERO).err().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    Timer::start(Mode::ITimer, Duration::from_millis(10)).unwrap();
}

fn current_handler(signal: c_int) -> libc::sighandler_t {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut action) },
        0
    );
    action.sa_sigaction
}

#[test]
fn handler_drop_ignores_signal() {
    let _serial = serial();
    assert_eq!(current_handler(libc::SIGUSR2), libc::SIG_DFL);
    let handler = SignalHandler::install(libc::SIGUSR2, on_signal).unwrap();
    assert_eq!(
        current_handler(libc::SIGUSR2),
        on_signal as *const () as usize
    );

    COUNT.store(0, Ordering::Relaxed);
    unsafe { libc::raise(libc::SIGUSR2) };
    assert_eq!(COUNT.load(Ordering::Relaxed), 1);

    drop(handler);
    assert_eq!(current_handler(libc::SIGUSR2), libc::SIG_IGN);
}

#[test]
fn invalid_signal_is_rejected() {
    assert!(SignalHandler::install(libc::SIGKILL, on_signal).is_err());
}
