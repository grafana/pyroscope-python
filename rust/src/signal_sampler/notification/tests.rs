use super::*;
use std::sync::{Barrier, mpsc};
use std::thread;

fn readable(n: &Notification) -> bool {
    let mut fd = libc::pollfd {
        fd: n.read.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut fd, 1, 0) == 1 }
}

#[test]
fn threshold_and_concurrent_coalescing() {
    let n = Notification::new().unwrap();
    n.threshold.swap(32, Ordering::AcqRel);
    n.published(|| 31);
    assert!(!readable(&n));
    thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                for _ in 0..1000 {
                    n.published(|| 32);
                }
            });
        }
    });
    let mut bytes = [0u8; 128];
    assert_eq!(
        unsafe { libc::read(n.read.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) },
        1
    );
    assert!(!readable(&n));
}

#[test]
fn publication_before_arm_and_between_recheck_and_poll() {
    let n = Notification::new().unwrap();
    let running = AtomicBool::new(true);
    n.published(|| 1); // Active consumer: no byte, but recheck must see the record.
    n.wait(1, None, &running, || 1).unwrap();
    assert!(!readable(&n));
    // Publication after the recheck's snapshot, but before entering poll.
    n.wait(1, Some(Duration::from_secs(2)), &running, || {
        n.published(|| 1);
        0
    })
    .unwrap();
    assert!(readable(&n));
}

#[test]
fn low_volume_timeout_and_idle_shutdown() {
    let n = Notification::new().unwrap();
    let running = AtomicBool::new(true);
    let start = Instant::now();
    n.wait(32, Some(MAX_DELAY), &running, || 1).unwrap();
    assert!(start.elapsed() >= MAX_DELAY);
    assert!(start.elapsed() < Duration::from_secs(2));
    thread::scope(|s| {
        s.spawn(|| {
            while n.threshold.load(Ordering::Acquire) != 1 {
                thread::yield_now();
            }
            running.store(false, Ordering::Release);
            n.wake();
        });
        n.wait(1, None, &running, || 0).unwrap();
    });
    assert!(!running.load(Ordering::Acquire));
    // Shutdown before arm: clearing the byte must not make the consumer sleep.
    n.wake();
    n.wait(1, None, &running, || 0).unwrap();
}

#[test]
fn full_pipe_is_nonblocking_and_preserves_errno() {
    let n = Notification::new().unwrap();
    let bytes = [0u8; 4096];
    while unsafe { libc::write(n.write.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) } > 0 {}
    while unsafe { libc::write(n.write.as_raw_fd(), bytes.as_ptr().cast(), 1) } == 1 {}
    assert_eq!(unsafe { *errno_pointer() }, libc::EAGAIN);
    unsafe {
        *errno_pointer() = libc::EDOM;
    }
    n.wake();
    assert_eq!(unsafe { *errno_pointer() }, libc::EDOM);
    assert!(readable(&n));
}

#[test]
fn unregister_waits_for_callbacks_and_restart_uses_fresh_pipe() {
    let registry = Registry::new();
    for _ in 0..100 {
        let n = Arc::new(Notification::new().unwrap());
        let weak = Arc::downgrade(&n);
        let registration = registry.register(n);
        let entered = Barrier::new(2);
        let release = Barrier::new(2);
        let (done_tx, done_rx) = mpsc::channel();
        thread::scope(|s| {
            s.spawn(|| {
                registry.with(|n| {
                    entered.wait();
                    release.wait();
                    n.wake();
                })
            });
            entered.wait();
            s.spawn(move || {
                drop(registration);
                done_tx.send(()).unwrap();
            });
            while !registry.pointer.load(Ordering::SeqCst).is_null() {
                thread::yield_now();
            }
            registry.with(|_| panic!("callback entered after unregistration"));
            assert!(weak.upgrade().is_some());
            assert!(done_rx.try_recv().is_err());
            release.wait();
            done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn sustained_producers_and_bounded_batches_deliver_exact_records() {
    use pyroscope_ringbuffer::RingBuffer;
    let ring = RingBuffer::<usize, 256>::new();
    let n = Notification::new().unwrap();
    let running = AtomicBool::new(true);
    let completed = AtomicUsize::new(0);
    thread::scope(|s| {
        for producer_id in 0..4 {
            let ring = &ring;
            let n = &n;
            let completed = &completed;
            s.spawn(move || {
                for sequence in 0..2000 {
                    let value = producer_id * 2000 + sequence;
                    loop {
                        if let Some(mut producer) = ring.producer()
                            && producer.push_slice(&[value]).is_ok()
                        {
                            break;
                        }
                        thread::yield_now();
                    }
                    n.published(|| ring.occupied_slots());
                }
                completed.fetch_add(1, Ordering::Release);
                n.wake();
            });
        }
        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while completed.load(Ordering::Acquire) != 4 || ring.occupied_slots() != 0 {
            assert!(
                Instant::now() < deadline,
                "consumer stopped making progress"
            );
            if ring.occupied_slots() == 0 {
                n.wait(1, Some(Duration::from_secs(1)), &running, || {
                    ring.occupied_slots()
                })
                .unwrap();
            }
            let mut consumer = ring.consumer().unwrap();
            for _ in 0..7 {
                let mut value = [0];
                if consumer.pop_slice(&mut value).is_err() {
                    break;
                }
                received.push(value[0]);
            }
        }
        received.sort_unstable();
        assert_eq!(received, (0..8000).collect::<Vec<_>>());
    });
}

static SIGNAL_REGISTRY: Registry = Registry::new();
static SIGNAL_COUNT: AtomicUsize = AtomicUsize::new(0);
extern "C" fn on_signal(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    SIGNAL_COUNT.fetch_add(1, Ordering::Relaxed);
    SIGNAL_REGISTRY.with(|n| n.published(|| 1));
}

#[test]
fn actual_signal_wakes_consumer_and_preserves_errno() {
    let _handler =
        pyroscope_signal_timer::SignalHandler::install(libc::SIGUSR1, on_signal).unwrap();
    let n = Arc::new(Notification::new().unwrap());
    let _registration = SIGNAL_REGISTRY.register(n.clone());
    n.threshold.swap(1, Ordering::AcqRel);
    unsafe {
        *errno_pointer() = libc::ERANGE;
        assert_eq!(libc::raise(libc::SIGUSR1), 0);
        assert_eq!(*errno_pointer(), libc::ERANGE);
    }
    assert!(readable(&n));
    n.clear().unwrap();
    n.threshold.swap(0, Ordering::AcqRel);
    let interrupted_thread = unsafe { libc::pthread_self() };
    let stop = AtomicBool::new(false);
    let count = SIGNAL_COUNT.load(Ordering::Relaxed);
    thread::scope(|s| {
        s.spawn(|| {
            let limit = Instant::now() + Duration::from_secs(2);
            while !stop.load(Ordering::Acquire) && Instant::now() < limit {
                unsafe {
                    libc::pthread_kill(interrupted_thread, libc::SIGUSR1);
                }
                thread::sleep(Duration::from_millis(1));
            }
        });
        let start = Instant::now();
        n.poll(Some(Duration::from_millis(50))).unwrap();
        stop.store(true, Ordering::Release);
        assert!(start.elapsed() >= Duration::from_millis(50));
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "EINTR restarted the deadline"
        );
    });
    assert!(SIGNAL_COUNT.load(Ordering::Relaxed) > count);
}

#[test]
fn shutdown_racing_rearm_never_sleeps() {
    for _ in 0..200 {
        let n = Notification::new().unwrap();
        let running = AtomicBool::new(true);
        let barrier = Barrier::new(2);
        thread::scope(|s| {
            s.spawn(|| {
                barrier.wait();
                running.store(false, Ordering::Release);
                n.wake();
            });
            barrier.wait();
            let start = Instant::now();
            n.wait(1, Some(Duration::from_secs(2)), &running, || 0)
                .unwrap();
            assert!(start.elapsed() < Duration::from_secs(1));
        });
    }
}
