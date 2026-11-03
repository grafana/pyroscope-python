use crate::{Consumer, Full, PopError, RingBuffer};
#[cfg(all(unix, not(miri)))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(all(unix, not(miri)))]
use std::time::{Duration, Instant};

#[test]
fn pop_returns_one_record() {
    let rb = RingBuffer::<usize, 8>::new();
    let mut p = rb.producer().unwrap();
    p.push_slice(&[1, 2, 3, 4, 5]).unwrap();
    p.push_slice(&[6]).unwrap();

    let mut c = rb.consumer().unwrap();
    let mut out = [0; 8];
    assert_eq!(c.pop_slice(&mut out), Ok(5));
    assert_eq!(out[..5], [1, 2, 3, 4, 5]);
    assert_eq!(c.pop_slice(&mut out), Ok(1));
    assert_eq!(out[0], 6);
    assert_eq!(c.pop_slice(&mut out), Err(PopError::Empty));
}

#[test]
fn empty_record() {
    let rb = RingBuffer::<usize, 4>::new();
    let mut p = rb.producer().unwrap();
    p.push_slice(&[]).unwrap();
    let mut c = rb.consumer().unwrap();
    assert_eq!(c.pop_slice(&mut []), Ok(0));
    assert_eq!(c.pop_slice(&mut []), Err(PopError::Empty));
}

#[test]
fn too_small_output_keeps_record() {
    let rb = RingBuffer::<usize, 4>::new();
    let mut p = rb.producer().unwrap();
    p.push_slice(&[1, 2, 3]).unwrap();
    let mut c = rb.consumer().unwrap();
    assert_eq!(
        c.pop_slice(&mut [0; 2]),
        Err(PopError::TooSmall { needed: 3 })
    );
    assert_eq!(drain(&mut c), vec![vec![1, 2, 3]]);
}

#[test]
fn full_is_all_or_nothing() {
    let rb = RingBuffer::<usize, 8>::new();
    let mut p = rb.producer().unwrap();
    p.push_slice(&[1, 2, 3]).unwrap();
    assert_eq!(p.push_slice(&[4, 5, 6, 7]), Err(Full));
    p.push_slice(&[4, 5]).unwrap();
    assert_eq!(p.push_slice(&[6]), Err(Full));

    let mut c = rb.consumer().unwrap();
    assert_eq!(c.pop_slice(&mut [0; 3]), Ok(3));
    p.push_slice(&[6, 7]).unwrap();
    assert_eq!(drain(&mut c), vec![vec![4, 5], vec![6, 7]]);
}

#[test]
fn empty_record_takes_a_slot() {
    let rb = RingBuffer::<usize, 2>::new();
    let mut p = rb.producer().unwrap();
    p.push_slice(&[]).unwrap();
    p.push_slice(&[]).unwrap();
    assert_eq!(p.push_slice(&[]), Err(Full));
}

#[test]
fn second_producer_or_consumer_is_refused() {
    let rb = RingBuffer::<usize, 4>::new();
    let p = rb.producer().unwrap();
    assert!(rb.producer().is_none());
    drop(p);
    assert!(rb.producer().is_some());

    let c = rb.consumer().unwrap();
    assert!(rb.consumer().is_none());
    assert!(rb.producer().is_some());
    drop(c);
    assert!(rb.consumer().is_some());
}

#[test]
fn oversized_record_leaves_buffer_unchanged() {
    let rb = RingBuffer::<usize, 4>::new();
    let mut p = rb.producer().unwrap();
    assert_eq!(p.push_slice(&[1, 2, 3, 4]), Err(Full));
    let mut c = rb.consumer().unwrap();
    assert_eq!(c.pop_slice(&mut [0; 4]), Err(PopError::Empty));
    p.push_slice(&[7, 8, 9]).unwrap();
    assert_eq!(p.push_slice(&[]), Err(Full));
    assert_eq!(drain(&mut c), vec![vec![7, 8, 9]]);
    p.push_slice(&[10]).unwrap();
    assert_eq!(drain(&mut c), vec![vec![10]]);
}

#[test]
fn wraps_machine_word_counter() {
    use std::sync::atomic::Ordering;
    let rb = RingBuffer::<usize, 8>::new();
    rb.head.store(usize::MAX - 2, Ordering::Relaxed);
    rb.tail.store(usize::MAX - 2, Ordering::Relaxed);
    let mut p = rb.producer().unwrap();
    let mut c = rb.consumer().unwrap();
    p.push_slice(&[1, 2, 3, 4]).unwrap();
    assert_eq!(drain(&mut c), vec![vec![1, 2, 3, 4]]);
    p.push_slice(&[5, 6]).unwrap();
    assert_eq!(drain(&mut c), vec![vec![5, 6]]);
}

#[test]
fn wraps_around() {
    let rb = RingBuffer::<u64, 8>::new();
    let mut c = rb.consumer().unwrap();
    for i in 0..1000u64 {
        let mut p = rb.producer().unwrap();
        p.push_slice(&[i, i + 1, i + 2]).unwrap();
        p.push_slice(&[i]).unwrap();
        assert_eq!(drain(&mut c), vec![vec![i, i + 1, i + 2], vec![i]]);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Frame {
    code: usize,
    line: u32,
}

fn record(n: usize) -> Vec<Frame> {
    (0..n % 7 + 1)
        .map(|i| Frame {
            code: n,
            line: i as u32,
        })
        .collect()
}

#[test]
fn concurrent_records_stay_intact() {
    const RECORDS: usize = if cfg!(miri) { 64 } else { 200_000 };
    static RB: RingBuffer<Frame, 64> = RingBuffer::new();

    let writer = std::thread::spawn(|| {
        let mut written = 0;
        while written < RECORDS {
            let Some(mut p) = RB.producer() else { continue };
            if p.push_slice(&record(written)).is_ok() {
                written += 1;
            }
        }
    });

    let mut c = RB.consumer().unwrap();
    let mut buf = [Frame::default(); 7];
    for n in 0..RECORDS {
        let len = loop {
            match c.pop_slice(&mut buf) {
                Ok(len) => break len,
                Err(e) => assert_eq!(e, PopError::Empty),
            }
        };
        assert_eq!(&buf[..len], record(n).as_slice());
    }
    writer.join().unwrap();
    assert_eq!(c.pop_slice(&mut buf), Err(PopError::Empty));
}

#[cfg(all(unix, not(miri)))]
#[test]
fn written_from_signal_handler() {
    static RB: RingBuffer<usize, 1024> = RingBuffer::new();
    static SIGNALS: AtomicUsize = AtomicUsize::new(0);
    static DROPPED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn handler(_: libc::c_int) {
        let n = SIGNALS.fetch_add(1, Ordering::Relaxed);
        let Some(mut p) = RB.producer() else {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let frames = [n; 5];
        if p.push_slice(&frames[..n % 5 + 1]).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }

    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handler as *const () as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGPROF, &sa, std::ptr::null_mut()), 0);
        let tick = libc::timeval {
            tv_sec: 0,
            tv_usec: 1000,
        };
        let timer = libc::itimerval {
            it_interval: tick,
            it_value: tick,
        };
        assert_eq!(
            libc::setitimer(libc::ITIMER_PROF, &timer, std::ptr::null_mut()),
            0
        );
    }

    let mut received = 0;
    let mut buf = [0usize; 5];
    let deadline = Instant::now() + Duration::from_secs(10);
    while received < 50 && Instant::now() < deadline {
        let Some(mut c) = RB.consumer() else { continue };
        while let Ok(len) = c.pop_slice(&mut buf) {
            let n = buf[0];
            assert_eq!(len, n % 5 + 1);
            assert!(buf[..len].iter().all(|&v| v == n));
            received += 1;
        }
    }

    unsafe {
        let off: libc::itimerval = std::mem::zeroed();
        libc::setitimer(libc::ITIMER_PROF, &off, std::ptr::null_mut());
    }
    assert!(
        received >= 50,
        "received {received} records, dropped {}",
        DROPPED.load(Ordering::Relaxed)
    );
}

fn drain<T: Copy + Default, const N: usize>(c: &mut Consumer<'_, T, N>) -> Vec<Vec<T>> {
    let mut buf = [T::default(); N];
    std::iter::from_fn(|| c.pop_slice(&mut buf).ok().map(|len| buf[..len].to_vec())).collect()
}
