//! A `SIGPROF` handler that unwinds the interrupted thread into a ring buffer.
//!
//! [`configure`] once with the running CPython's [`Layout`], install [`handler`], and drain with
//! [`consumer`]. Each record is the frames of the interrupted thread, innermost first.

use super::{Error, Frame, Layout, ThreadStateGetter, Unwinder};
use crate::signal_sampler::ringbuffer::{Consumer, RingBuffer};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

const SLOTS: usize = 1 << 16;

pub type Handler = extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut c_void);

trait Unwind: Sync {
    fn unwind(&self, records: &RingBuffer<Frame, SLOTS>) -> Result<usize, Error>;
}

impl<L: Layout> Unwind for Unwinder<L> {
    fn unwind(&self, records: &RingBuffer<Frame, SLOTS>) -> Result<usize, Error> {
        Unwinder::unwind(self, records)
    }
}

static RECORDS: RingBuffer<Frame, SLOTS> = RingBuffer::new();
// A fat pointer does not fit in an atomic, so this points to a leaked one.
static UNWINDER: AtomicPtr<&'static dyn Unwind> = AtomicPtr::new(ptr::null_mut());

/// Sets what the handler needs. Until it is called the handler does nothing.
///
/// `code_type` is the address of `PyCode_Type`. `max_frames` bounds each walk. Configurations
/// are leaked and never freed, since a handler may still be using a replaced one.
pub fn configure<L: Layout>(code_type: usize, thread_state: ThreadStateGetter, max_frames: usize) {
    let unwinder: &'static dyn Unwind = Box::leak(Box::new(Unwinder::<L>::new(
        code_type,
        thread_state,
        max_frames,
    )));
    UNWINDER.store(Box::leak(Box::new(unwinder)), Ordering::Release);
}

/// Reads the records the handler wrote. `None` if another consumer is active.
pub fn consumer() -> Option<Consumer<'static, Frame, SLOTS>> {
    RECORDS.consumer()
}

fn unwinder() -> Option<&'static dyn Unwind> {
    let unwinder = UNWINDER.load(Ordering::Acquire);
    // SAFETY: `configure` stored a leaked reference, which is never freed.
    (!unwinder.is_null()).then(|| unsafe { *unwinder })
}

pub extern "C" fn handler(_: libc::c_int, _: *mut libc::siginfo_t, _: *mut c_void) {
    if let Some(unwinder) = unwinder() {
        on_signal(unwinder);
    }
}

fn on_signal(unwinder: &dyn Unwind) {
    // Errors are counted by the unwinder.
    let _ = unwinder.unwind(&RECORDS);
}
