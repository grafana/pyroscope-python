//! A `SIGPROF` handler that unwinds the interrupted thread into a ring buffer.
//!
//! Call [`super::init`], install [`handler`], and drain with [`consumer`]. Each record contains
//! the frames of the interrupted thread, innermost first.

use super::{Frame, MAX_FRAMES, unwind};
use crate::signal_sampler::ringbuffer::{Consumer, RingBuffer};
use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicUsize, Ordering};

const SLOTS: usize = 1 << 16;

pub type Handler = extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut c_void);

static RECORDS: RingBuffer<Frame, SLOTS> = RingBuffer::new();
static CONTENTIONS: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);

/// Reads the records the handler wrote. `None` if another consumer is active.
pub fn consumer() -> Option<Consumer<'static, Frame, SLOTS>> {
    RECORDS.consumer()
}

pub extern "C" fn handler(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut c_void) {
    let mut frames = [MaybeUninit::uninit(); MAX_FRAMES];
    // Unwind errors are counted by the unwinder. Never publish a partial stack.
    let Ok(count) = unwind(&mut frames) else {
        return;
    };
    let Some(mut producer) = RECORDS.producer() else {
        CONTENTIONS.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let mut record = producer.record();
    for frame in &frames[..count] {
        // SAFETY: a successful unwind initialized exactly the first `count` frames.
        let frame = unsafe { frame.assume_init() };
        if record.push(frame).is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
    if record.commit().is_err() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}
