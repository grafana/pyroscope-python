//! Stress tests: many threads writing and reading, and many threads writing from signal handlers.

use crate::{Consumer, PopError, Producer, RingBuffer};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Frame {
    code: usize,
    line: u32,
}

const MAX_LEN: usize = 7;

fn len_of(id: usize) -> usize {
    id % MAX_LEN + 1
}

fn push(p: &mut Producer<'_, Frame, 64>, id: usize) -> bool {
    let mut r = p.record();
    for line in 0..len_of(id) {
        let frame = Frame {
            code: id,
            line: line as u32,
        };
        if r.push(frame).is_err() {
            return false;
        }
    }
    r.commit().is_ok()
}

/// Returns the id of a popped record after checking that it is intact.
fn pop(c: &mut Consumer<'_, Frame, 64>) -> Result<usize, PopError> {
    let mut buf = [Frame::default(); MAX_LEN];
    let len = c.pop_slice(&mut buf)?;
    let id = buf[0].code;
    assert_eq!(len, len_of(id), "record {id} has a wrong length");
    for (line, frame) in buf[..len].iter().enumerate() {
        assert_eq!(
            *frame,
            Frame {
                code: id,
                line: line as u32
            },
            "record {id} is torn"
        );
    }
    Ok(id)
}

const PRODUCERS: usize = 4;
const RECORDS: usize = 50_000;
const TOTAL: usize = PRODUCERS * RECORDS;

fn produce_all(rb: &'static RingBuffer<Frame, 64>) -> Vec<std::thread::JoinHandle<()>> {
    (0..PRODUCERS)
        .map(|producer| {
            std::thread::spawn(move || {
                let deadline = Instant::now() + TIMEOUT;
                for seq in 0..RECORDS {
                    let id = producer * RECORDS + seq;
                    loop {
                        assert!(Instant::now() < deadline, "producer {producer} timed out");
                        let Some(mut p) = rb.producer() else {
                            std::thread::yield_now();
                            continue;
                        };
                        if push(&mut p, id) {
                            break;
                        }
                    }
                }
            })
        })
        .collect()
}

#[test]
fn many_producers_one_consumer_keep_per_producer_order() {
    static RB: RingBuffer<Frame, 64> = RingBuffer::new();

    let producers = produce_all(&RB);
    let mut next = [0usize; PRODUCERS];
    let mut c = RB.consumer().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    for _ in 0..TOTAL {
        let id = loop {
            assert!(Instant::now() < deadline, "consumer timed out");
            match pop(&mut c) {
                Ok(id) => break id,
                Err(e) => assert_eq!(e, PopError::Empty),
            }
        };
        let (producer, seq) = (id / RECORDS, id % RECORDS);
        assert_eq!(
            seq, next[producer],
            "producer {producer} reordered or lost a record"
        );
        next[producer] += 1;
    }
    for p in producers {
        p.join().unwrap();
    }
    assert_eq!(pop(&mut c), Err(PopError::Empty));
}

#[test]
fn many_producers_many_consumers_deliver_each_record_once() {
    const CONSUMERS: usize = 3;
    static RB: RingBuffer<Frame, 64> = RingBuffer::new();
    static SEEN: [AtomicU8; TOTAL] = [const { AtomicU8::new(0) }; TOTAL];
    static RECEIVED: AtomicUsize = AtomicUsize::new(0);

    let producers = produce_all(&RB);
    let consumers: Vec<_> = (0..CONSUMERS)
        .map(|consumer| {
            std::thread::spawn(move || {
                let deadline = Instant::now() + TIMEOUT;
                while RECEIVED.load(Ordering::Relaxed) < TOTAL {
                    assert!(Instant::now() < deadline, "consumer {consumer} timed out");
                    let Some(mut c) = RB.consumer() else {
                        std::thread::yield_now();
                        continue;
                    };
                    while let Ok(id) = pop(&mut c) {
                        assert_eq!(SEEN[id].fetch_add(1, Ordering::Relaxed), 0, "{id} twice");
                        RECEIVED.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    for t in producers.into_iter().chain(consumers) {
        t.join().unwrap();
    }
    assert_eq!(RECEIVED.load(Ordering::Relaxed), TOTAL);
    assert!(SEEN.iter().all(|s| s.load(Ordering::Relaxed) == 1));
}

#[cfg(all(unix, not(miri)))]
#[test]
fn many_threads_writing_from_signal_handlers() {
    const THREADS: usize = 4;
    const SIGNALS: usize = 20_000;
    static RB: RingBuffer<Frame, 64> = RingBuffer::new();
    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
    static WRITTEN: AtomicUsize = AtomicUsize::new(0);
    static DROPPED: AtomicUsize = AtomicUsize::new(0);
    static SEEN: [AtomicU8; THREADS * SIGNALS * 2] =
        [const { AtomicU8::new(0) }; THREADS * SIGNALS * 2];

    thread_local! {
        static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    // Each signal takes the producer lock, then raises a second signal on the same thread
    // before it commits, so the nested handler finds the lock taken.
    extern "C" fn handler(_: libc::c_int) {
        let depth = DEPTH.with(|d| d.replace(d.get() + 1));
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        match RB.producer() {
            Some(mut p) => {
                let mut r = p.record();
                let half = len_of(id) / 2;
                let mut ok = true;
                for line in 0..len_of(id) {
                    if line == half && depth == 0 {
                        unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGUSR1) };
                    }
                    let frame = Frame {
                        code: id,
                        line: line as u32,
                    };
                    ok &= r.push(frame).is_ok();
                }
                if ok && r.commit().is_ok() {
                    WRITTEN.fetch_add(1, Ordering::Relaxed);
                } else {
                    DROPPED.fetch_add(1, Ordering::Relaxed);
                }
            }
            None => {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
        DEPTH.with(|d| d.set(depth));
    }

    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler as *const () as usize;
        // SA_NODEFER lets the nested signal run inside the handler.
        sa.sa_flags = libc::SA_RESTART | libc::SA_NODEFER;
        libc::sigemptyset(&mut sa.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()), 0);
    }

    let senders: Vec<_> = (0..THREADS)
        .map(|_| {
            std::thread::spawn(|| {
                for _ in 0..SIGNALS {
                    unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGUSR1) };
                }
            })
        })
        .collect();

    let mut received = 0;
    let mut c = RB.consumer().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let mut drain = |c: &mut Consumer<'_, Frame, 64>| {
        while let Ok(id) = pop(c) {
            assert_eq!(SEEN[id].fetch_add(1, Ordering::Relaxed), 0, "{id} twice");
            received += 1;
        }
    };
    while !senders.iter().all(|s| s.is_finished()) {
        assert!(Instant::now() < deadline, "senders timed out");
        drain(&mut c);
    }
    for s in senders {
        s.join().unwrap();
    }
    drain(&mut c);

    let written = WRITTEN.load(Ordering::Relaxed);
    let dropped = DROPPED.load(Ordering::Relaxed);
    // Every handler run, nested ones included, either wrote a record or counted a drop.
    assert_eq!(written + dropped, NEXT_ID.load(Ordering::Relaxed));
    assert_eq!(received, written);
    assert!(dropped > 0, "no handler was ever refused the producer lock");
    assert!(written > 0, "no handler wrote a record");
}
