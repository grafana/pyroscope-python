//! Lock-free ring buffer that can be written from a signal handler and read from a normal thread.
//!
//! Each slice passed to [`Producer::push_slice`] is one record, and each [`Consumer::pop_slice`]
//! returns one record.
//! Both sides take a try-lock, so a nested or concurrent signal handler fails to get a
//! [`Producer`] instead of corrupting the buffer.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// Release targets have lock-free pointer and byte atomics. These operations run in a signal handler.
#[cfg(not(all(target_has_atomic = "ptr", target_has_atomic = "8")))]
compile_error!("the signal ring buffer requires lock-free pointer and byte atomics");
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("the signal ring buffer is validated only on x86_64 and aarch64");

const SLEEPING: usize = 1 << (usize::BITS - 1);
const INDEX: usize = !SLEEPING;

/// Holds `N` slots. A record takes one slot for its length plus one slot per value.
pub struct RingBuffer<T: Copy, const N: usize> {
    slots: [UnsafeCell<MaybeUninit<Slot<T>>>; N],
    head: AtomicUsize,
    tail: AtomicUsize,
    producer_locked: AtomicBool,
    consumer_locked: AtomicBool,
}

#[derive(Clone, Copy)]
enum Slot<T: Copy> {
    Len(usize),
    Value(T),
}

// SAFETY: a slot is accessed by the producer only while it is free and by the consumer only
// while it is published; head and tail hand slots over with release/acquire ordering.
unsafe impl<T: Copy + Send, const N: usize> Sync for RingBuffer<T, N> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopError {
    Empty,
    /// The next record is longer than the output slice. It stays in the buffer.
    TooSmall {
        needed: usize,
    },
}

impl<T: Copy, const N: usize> RingBuffer<T, N> {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        const { assert!(N.is_power_of_two()) };
        RingBuffer {
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; N],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            producer_locked: AtomicBool::new(false),
            consumer_locked: AtomicBool::new(false),
        }
    }

    /// Returns `None` if another producer is active, e.g. when called from a nested signal handler.
    pub fn producer(&self) -> Option<Producer<'_, T, N>> {
        if !try_lock(&self.producer_locked) {
            return None;
        }
        Some(Producer {
            head: self.head.load(Ordering::Relaxed) & INDEX,
            tail: self.tail.load(Ordering::Acquire),
            rb: self,
        })
    }

    pub fn consumer(&self) -> Option<Consumer<'_, T, N>> {
        if !try_lock(&self.consumer_locked) {
            return None;
        }
        Some(Consumer {
            head: self.head.load(Ordering::Acquire) & INDEX,
            tail: self.tail.load(Ordering::Relaxed),
            rb: self,
        })
    }

    fn slot(&self, index: usize) -> *mut MaybeUninit<Slot<T>> {
        self.slots[index & (N - 1)].get()
    }

    /// Arm the reader only when all published records have been consumed. A concurrent
    /// publication changes the index, causing the compare-exchange to fail.
    pub fn arm_sleep(&self) -> bool {
        let head = self.head.load(Ordering::Acquire);
        if head & INDEX != self.tail.load(Ordering::Acquire) {
            return false;
        }
        self.head
            .compare_exchange(head, head | SLEEPING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn is_empty(&self) -> bool {
        self.head.load(Ordering::Acquire) & INDEX == self.tail.load(Ordering::Acquire)
    }

    pub fn published_index(&self) -> usize {
        self.head.load(Ordering::Acquire) & INDEX
    }

    pub fn consumed_through(&self, index: usize) -> bool {
        (self.tail.load(Ordering::Acquire).wrapping_sub(index) & INDEX) < INDEX / 2
    }

    fn wakeup_extra(&self) -> bool {
        self.head.fetch_and(INDEX, Ordering::AcqRel) & SLEEPING != 0
    }

    /// Clear a pending sleep when the producer is ending. The owner must wake
    /// the reader if this returns true.
    pub fn end_wakeup(&self) -> bool {
        self.wakeup_extra()
    }
}

fn try_lock(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Exclusive write access.
pub struct Producer<'a, T: Copy, const N: usize> {
    rb: &'a RingBuffer<T, N>,
    head: usize,
    tail: usize,
}

impl<T: Copy, const N: usize> Producer<'_, T, N> {
    /// Publishes `values` as one record. On `Full`, nothing is written.
    pub fn push_slice(&mut self, values: &[T]) -> Result<(), Full> {
        self.push_slice_and_check_wakeup(values).map(|_| ())
    }

    /// Returns whether this publication claimed a sleeping reader's wakeup.
    pub fn push_slice_and_check_wakeup(&mut self, values: &[T]) -> Result<bool, Full> {
        if !self.fits(values.len()) {
            self.tail = self.rb.tail.load(Ordering::Acquire);
            if !self.fits(values.len()) {
                return Err(Full);
            }
        }
        for (i, &value) in values.iter().enumerate() {
            let index = self.head.wrapping_add(1).wrapping_add(i);
            // SAFETY: the entire record fits in free slots and we hold the producer lock.
            // The consumer cannot read these slots until the release store below.
            unsafe { (*self.rb.slot(index)).write(Slot::Value(value)) };
        }
        // SAFETY: the length slot is free too; publish only after all values are written.
        unsafe { (*self.rb.slot(self.head)).write(Slot::Len(values.len())) };
        self.head = self.head.wrapping_add(1).wrapping_add(values.len()) & INDEX;
        let mut old = self.rb.head.load(Ordering::Acquire);
        loop {
            let unread = self.head.wrapping_sub(self.rb.tail.load(Ordering::Acquire)) & INDEX;
            let wake = unread >= N / 2 && old & SLEEPING != 0;
            let new = self.head | (if unread < N / 2 { old & SLEEPING } else { 0 });
            match self.rb.head.compare_exchange_weak(old, new, Ordering::Release, Ordering::Acquire) {
                Ok(_) => return Ok(wake),
                Err(actual) => old = actual,
            }
        }
    }

    /// A full buffer must wake a sleeping reader even if no record was published.
    pub fn wake_on_full(&self) -> bool {
        self.rb.wakeup_extra()
    }

    fn fits(&self, len: usize) -> bool {
        N - (self.head.wrapping_sub(self.tail) & INDEX) > len
    }
}

impl<T: Copy, const N: usize> Drop for Producer<'_, T, N> {
    fn drop(&mut self) {
        self.rb.producer_locked.store(false, Ordering::Release);
    }
}

pub struct Consumer<'a, T: Copy, const N: usize> {
    rb: &'a RingBuffer<T, N>,
    head: usize,
    tail: usize,
}

impl<T: Copy, const N: usize> Consumer<'_, T, N> {
    /// Copies the next record into the start of `out` and returns its length.
    pub fn pop_slice(&mut self, out: &mut [T]) -> Result<usize, PopError> {
        if self.tail == self.head {
            self.head = self.rb.head.load(Ordering::Acquire) & INDEX;
            if self.tail == self.head {
                return Err(PopError::Empty);
            }
        }
        // SAFETY: the record is published and the producer does not reuse its slots until tail
        // moves past them.
        let Slot::Len(len) = (unsafe { (*self.rb.slot(self.tail)).assume_init_read() }) else {
            unreachable!("record does not start with a length");
        };
        if len > out.len() {
            return Err(PopError::TooSmall { needed: len });
        }
        let start = self.tail.wrapping_add(1);
        for (i, out) in out[..len].iter_mut().enumerate() {
            let Slot::Value(value) =
                (unsafe { (*self.rb.slot(start.wrapping_add(i))).assume_init_read() })
            else {
                unreachable!("record value slot holds a length");
            };
            *out = value;
        }
        self.tail = start.wrapping_add(len) & INDEX;
        self.rb.tail.store(self.tail, Ordering::Release);
        Ok(len)
    }
}

impl<T: Copy, const N: usize> Drop for Consumer<'_, T, N> {
    fn drop(&mut self) {
        self.rb.consumer_locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
