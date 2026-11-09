//! Lock-free ring buffer that can be written from a signal handler and read from a normal thread.
//!
//! Each slice passed to [`Producer::push_slice`] is one record, and each [`Consumer::pop_slice`]
//! returns one record.
//! Both sides take a try-lock, so a nested or concurrent signal handler fails to get a
//! [`Producer`] instead of corrupting the buffer.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Holds `N` slots. A record takes one slot for its length and metadata plus one slot per value.
/// `M` is optional per-record metadata, published atomically with the values.
pub struct RingBuffer<T: Copy, const N: usize, M: Copy = ()> {
    slots: [UnsafeCell<MaybeUninit<Slot<T, M>>>; N],
    head: AtomicUsize,
    tail: AtomicUsize,
    producer_locked: AtomicBool,
    consumer_locked: AtomicBool,
}

#[derive(Clone, Copy)]
enum Slot<T: Copy, M: Copy> {
    Len { len: usize, metadata: M },
    Value(T),
}

// SAFETY: a slot is accessed by the producer only while it is free and by the consumer only
// while it is published; head and tail hand slots over with release/acquire ordering.
unsafe impl<T: Copy + Send, const N: usize, M: Copy + Send> Sync for RingBuffer<T, N, M> {}

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

impl<T: Copy, const N: usize, M: Copy> RingBuffer<T, N, M> {
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
    pub fn producer(&self) -> Option<Producer<'_, T, N, M>> {
        if !try_lock(&self.producer_locked) {
            return None;
        }
        Some(Producer {
            head: self.head.load(Ordering::Relaxed),
            tail: self.tail.load(Ordering::Acquire),
            rb: self,
        })
    }

    pub fn consumer(&self) -> Option<Consumer<'_, T, N, M>> {
        if !try_lock(&self.consumer_locked) {
            return None;
        }
        Some(Consumer {
            head: self.head.load(Ordering::Acquire),
            tail: self.tail.load(Ordering::Relaxed),
            rb: self,
        })
    }

    fn slot(&self, index: usize) -> *mut MaybeUninit<Slot<T, M>> {
        self.slots[index & (N - 1)].get()
    }
}

fn try_lock(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_ok()
}

/// Exclusive write access.
pub struct Producer<'a, T: Copy, const N: usize, M: Copy = ()> {
    rb: &'a RingBuffer<T, N, M>,
    head: usize,
    tail: usize,
}

impl<T: Copy, const N: usize, M: Copy> Producer<'_, T, N, M> {
    /// Publishes `values` as one record. On `Full`, nothing is written.
    pub fn push_slice(&mut self, values: &[T], metadata: M) -> Result<(), Full> {
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
        unsafe {
            (*self.rb.slot(self.head)).write(Slot::Len {
                len: values.len(),
                metadata,
            })
        };
        self.head = self.head.wrapping_add(1).wrapping_add(values.len());
        self.rb.head.store(self.head, Ordering::Release);
        Ok(())
    }

    fn fits(&self, len: usize) -> bool {
        N - self.head.wrapping_sub(self.tail) > len
    }
}

impl<T: Copy, const N: usize, M: Copy> Drop for Producer<'_, T, N, M> {
    fn drop(&mut self) {
        self.rb.producer_locked.store(false, Ordering::Release);
    }
}

pub struct Consumer<'a, T: Copy, const N: usize, M: Copy = ()> {
    rb: &'a RingBuffer<T, N, M>,
    head: usize,
    tail: usize,
}

impl<T: Copy, const N: usize, M: Copy> Consumer<'_, T, N, M> {
    /// Copies the next record into `out` and returns its length and metadata.
    pub fn pop_slice(&mut self, out: &mut [T]) -> Result<(usize, M), PopError> {
        if self.tail == self.head {
            self.head = self.rb.head.load(Ordering::Acquire);
            if self.tail == self.head {
                return Err(PopError::Empty);
            }
        }
        // SAFETY: the record is published and the producer does not reuse its slots until tail
        // moves past them.
        let Slot::Len { len, metadata } =
            (unsafe { (*self.rb.slot(self.tail)).assume_init_read() })
        else {
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
        self.tail = start.wrapping_add(len);
        self.rb.tail.store(self.tail, Ordering::Release);
        Ok((len, metadata))
    }
}

impl<T: Copy, const N: usize, M: Copy> Drop for Consumer<'_, T, N, M> {
    fn drop(&mut self) {
        self.rb.consumer_locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
