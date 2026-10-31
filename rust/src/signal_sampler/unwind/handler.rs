//! A `SIGPROF` handler that unwinds the interrupted thread into a ring buffer.
//!
//! [`configure`] once, install [`handler`], and drain with [`consumer`]. Each record is the
//! frames of the interrupted thread, innermost first.

use super::{Frame, ThreadStateGetter, Unwinder};
use crate::signal_sampler::ringbuffer::{Consumer, RingBuffer};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

const SLOTS: usize = 1 << 16;

pub type Handler = extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut c_void);

static RECORDS: RingBuffer<Frame, SLOTS> = RingBuffer::new();
static UNWINDER: AtomicPtr<Unwinder> = AtomicPtr::new(ptr::null_mut());

/// Sets what the handler needs. Until it is called the handler does nothing.
///
/// `code_type` is the address of `PyCode_Type`. [`super::MAX_FRAMES`] bounds each walk. Configurations
/// are leaked and never freed, since a handler may still be using a replaced one.
pub fn configure(code_type: usize, thread_state: ThreadStateGetter) {
    let unwinder = Box::leak(Box::new(Unwinder::new(code_type, thread_state)));
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
        // Errors are counted by the unwinder.
        let _ = unwinder.unwind(&RECORDS);
    }
}
