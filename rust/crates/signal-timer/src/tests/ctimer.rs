#![cfg(target_os = "linux")]

use super::common::{burn_cpu, serial};
use crate::{Mode, SignalHandler, Timer, c_int, gettid, siginfo_t};
use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Barrier, mpsc};
use std::thread;
use std::time::Duration;

const SIGNAL: c_int = Timer::SIGNAL;
const INTERVAL: Duration = Duration::from_millis(1);

static BUSY_TID: AtomicI32 = AtomicI32::new(0);
static IDLE_TID: AtomicI32 = AtomicI32::new(0);
static BUSY: AtomicUsize = AtomicUsize::new(0);
static IDLE: AtomicUsize = AtomicUsize::new(0);
static OTHER: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_signal(_: c_int, _: *mut siginfo_t, _: *mut c_void) {
    let tid = gettid();
    let counter = if tid == BUSY_TID.load(Ordering::Relaxed) {
        &BUSY
    } else if tid == IDLE_TID.load(Ordering::Relaxed) {
        &IDLE
    } else {
        &OTHER
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

fn reset(busy: i32, idle: i32) {
    BUSY_TID.store(busy, Ordering::Relaxed);
    IDLE_TID.store(idle, Ordering::Relaxed);
    for c in [&BUSY, &IDLE, &OTHER] {
        c.store(0, Ordering::Relaxed);
    }
}

#[test]
fn signals_follow_each_threads_cpu_time() {
    let _serial = serial();
    let _handler = SignalHandler::install(SIGNAL, on_signal).unwrap();

    let start = Barrier::new(3);
    let (tx, rx) = mpsc::channel();
    thread::scope(|s| {
        let busy_thread = s.spawn(|| {
            tx.send(("busy", gettid())).unwrap();
            start.wait();
            burn_cpu(Duration::from_millis(200));
        });
        let idle_thread = s.spawn(|| {
            tx.send(("idle", gettid())).unwrap();
            start.wait();
            thread::sleep(Duration::from_millis(200));
        });
        let mut tids = [rx.recv().unwrap(), rx.recv().unwrap()];
        tids.sort();
        let [("busy", busy), ("idle", idle)] = tids else {
            unreachable!()
        };
        reset(busy, idle);
        let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
        timer.thread_created(busy).unwrap();
        timer.thread_created(idle).unwrap();
        start.wait();
        busy_thread.join().unwrap();
        idle_thread.join().unwrap();
    });

    let busy = BUSY.load(Ordering::Relaxed);
    assert!(busy >= 100, "busy thread got {busy} signals");
    assert_eq!(IDLE.load(Ordering::Relaxed), 0);
    assert_eq!(OTHER.load(Ordering::Relaxed), 0);
}

#[test]
fn main_thread_burning_cpu_does_not_signal_other_threads() {
    let _serial = serial();
    let _handler = SignalHandler::install(SIGNAL, on_signal).unwrap();

    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let idle = thread::spawn(move || {
        tx.send(gettid()).unwrap();
        done_rx.recv().unwrap();
    });
    let idle_tid = rx.recv().unwrap();
    reset(gettid(), idle_tid);

    let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
    timer.thread_created(idle_tid).unwrap();
    burn_cpu(Duration::from_millis(100));
    done_tx.send(()).unwrap();
    idle.join().unwrap();

    assert_eq!(BUSY.load(Ordering::Relaxed), 0);
    assert_eq!(IDLE.load(Ordering::Relaxed), 0);
    assert_eq!(OTHER.load(Ordering::Relaxed), 0);
}

#[test]
fn no_signals_before_thread_created() {
    let _serial = serial();
    let _handler = SignalHandler::install(SIGNAL, on_signal).unwrap();
    reset(gettid(), 0);

    let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
    assert_eq!(timer.mode(), Mode::CTimer);
    burn_cpu(Duration::from_millis(50));
    assert_eq!(BUSY.load(Ordering::Relaxed), 0);
    assert_eq!(OTHER.load(Ordering::Relaxed), 0);
}

#[test]
fn stops_on_drop() {
    let _serial = serial();
    let _handler = SignalHandler::install(SIGNAL, on_signal).unwrap();
    reset(gettid(), 0);

    let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
    timer.thread_created(gettid()).unwrap();
    burn_cpu(Duration::from_millis(100));
    drop(timer);
    let fired = BUSY.load(Ordering::Relaxed);
    assert!(fired >= 50, "fired {fired} times");

    burn_cpu(Duration::from_millis(50));
    assert_eq!(BUSY.load(Ordering::Relaxed), fired);
}

#[test]
fn thread_removed_stops_its_signals() {
    let _serial = serial();
    let _handler = SignalHandler::install(SIGNAL, on_signal).unwrap();
    reset(gettid(), 0);

    let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
    timer.thread_created(gettid()).unwrap();
    timer.thread_created(gettid()).unwrap();
    burn_cpu(Duration::from_millis(100));
    let fired = BUSY.load(Ordering::Relaxed);
    assert!((50..=150).contains(&fired), "fired {fired} times");

    timer.thread_removed(gettid());
    let fired = BUSY.load(Ordering::Relaxed);
    burn_cpu(Duration::from_millis(50));
    assert_eq!(BUSY.load(Ordering::Relaxed), fired);

    timer.thread_removed(gettid());
    timer.thread_created(gettid()).unwrap();
    burn_cpu(Duration::from_millis(50));
    assert!(BUSY.load(Ordering::Relaxed) > fired);
}

#[test]
fn removed_after_thread_exit() {
    let _serial = serial();
    let _handler = SignalHandler::install(SIGNAL, on_signal).unwrap();

    let (tx, rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let t = thread::spawn(move || {
        tx.send(gettid()).unwrap();
        go_rx.recv().unwrap();
        burn_cpu(Duration::from_millis(50));
    });
    let tid = rx.recv().unwrap();
    reset(tid, 0);
    let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
    timer.thread_created(tid).unwrap();
    go_tx.send(()).unwrap();
    t.join().unwrap();
    timer.thread_removed(tid);
    assert!(BUSY.load(Ordering::Relaxed) >= 20);
    assert_eq!(OTHER.load(Ordering::Relaxed), 0);
}

#[test]
fn only_one_timer_at_a_time() {
    let _serial = serial();
    let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
    for mode in [Mode::CTimer, Mode::ITimer] {
        let err = Timer::start(mode, INTERVAL).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    }
    drop(timer);
    drop(Timer::start(Mode::ITimer, INTERVAL).unwrap());
}

#[test]
fn unknown_thread_is_rejected() {
    let _serial = serial();
    let timer = Timer::start(Mode::CTimer, INTERVAL).unwrap();
    let err = timer.thread_created(0x0fff_ffff).err().unwrap();
    assert_eq!(err.raw_os_error(), Some(libc::EINVAL));
}

#[test]
fn zero_interval_is_rejected() {
    let _serial = serial();
    let err = Timer::start(Mode::CTimer, Duration::ZERO).err().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}
