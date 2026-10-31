//! A `SIGPROF` handler that unwinds the interrupted thread into a ring buffer.
//!
//! [`configure`] once, install [`handler`], and drain with [`consumer`]. Each record is the
//! frames of the interrupted thread, innermost first.

use super::{Frame, MAX_FRAMES, Unwinder};
use crate::signal_sampler::ringbuffer::{Consumer, RingBuffer};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

const SLOTS: usize = 1 << 16;

pub type Handler = extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut c_void);

static RECORDS: RingBuffer<Frame, SLOTS> = RingBuffer::new();
static UNWINDER: AtomicPtr<Unwinder> = AtomicPtr::new(ptr::null_mut());
static CONTENTIONS: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);

/// Sets what the handler needs. Until it is called the handler does nothing.
///
/// [`super::MAX_FRAMES`] bounds each walk. Configurations
/// are leaked and never freed, since a handler may still be using a replaced one.
pub fn configure() {
    let unwinder = Box::leak(Box::new(Unwinder::new()));
    UNWINDER.store(unwinder, Ordering::Release);
}

/// Reads the records the handler wrote. `None` if another consumer is active.
pub fn consumer() -> Option<Consumer<'static, Frame, SLOTS>> {
    RECORDS.consumer()
}

fn unwinder() -> Option<&'static Unwinder> {
    // SAFETY: `configure` stored a leaked box, which is never freed.
    unsafe { UNWINDER.load(Ordering::Acquire).as_ref() }
}

pub extern "C" fn handler(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut c_void) {
    if let Some(unwinder) = unwinder() {
        let mut frames = [Frame::default(); MAX_FRAMES];
        // Unwind errors are counted by the unwinder. Never publish a partial stack.
        let Ok(count) = unwinder.unwind(&mut frames) else {
            return;
        };
        let Some(mut producer) = RECORDS.producer() else {
            CONTENTIONS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let mut record = producer.record();
        for frame in &frames[..count] {
            if record.push(*frame).is_err() {
                DROPPED.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        if record.commit().is_err() {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
}
