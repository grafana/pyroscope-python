//! Lock-free ring buffer that can be written from a signal handler and read from a normal thread.
//!
//! Each [`Record`] committed through a [`Producer`] is one record, and each [`Consumer::pop_slice`]
//! returns one record.
//! Both sides take a try-lock, so a nested or concurrent signal handler fails to get a
//! [`Producer`] instead of corrupting the buffer.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
            head: self.head.load(Ordering::Relaxed),
            tail: self.tail.load(Ordering::Acquire),
            rb: self,
        })
    }

    pub fn consumer(&self) -> Option<Consumer<'_, T, N>> {
        if !try_lock(&self.consumer_locked) {
            return None;
        }
        Some(Consumer {
            head: self.head.load(Ordering::Acquire),
            tail: self.tail.load(Ordering::Relaxed),
            rb: self,
        })
    }

    fn slot(&self, index: usize) -> *mut MaybeUninit<Slot<T>> {
        self.slots[index & (N - 1)].get()
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

impl<'a, T: Copy, const N: usize> Producer<'a, T, N> {
    /// Starts a record that is written one value at a time.
    pub fn record(&mut self) -> Record<'_, 'a, T, N> {
        Record {
            producer: self,
            len: 0,
        }
    }

    fn fits(&self, len: usize) -> bool {
        N - self.head.wrapping_sub(self.tail) > len
    }

    fn fits_refreshed(&mut self, len: usize) -> bool {
        if !self.fits(len) {
            self.tail = self.rb.tail.load(Ordering::Acquire);
        }
        self.fits(len)
    }
}

/// A record being written. The consumer sees nothing until [`Record::commit`]; dropping it
/// discards the values pushed so far.
pub struct Record<'p, 'a, T: Copy, const N: usize> {
    producer: &'p mut Producer<'a, T, N>,
    len: usize,
}

impl<T: Copy, const N: usize> Record<'_, '_, T, N> {
    pub fn push(&mut self, value: T) -> Result<(), Full> {
        let p = &mut *self.producer;
        if !p.fits_refreshed(self.len + 1) {
            return Err(Full);
        }
        let index = p.head.wrapping_add(1 + self.len);
        // SAFETY: the slot is past the published head and before tail + N, so the consumer does
        // not read it, and we hold the producer lock.
        unsafe { (*p.rb.slot(index)).write(Slot::Value(value)) };
        self.len += 1;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn commit(self) -> Result<(), Full> {
        let p = self.producer;
        if !p.fits_refreshed(self.len) {
            return Err(Full);
        }
        unsafe { (*p.rb.slot(p.head)).write(Slot::Len(self.len)) };
        p.head = p.head.wrapping_add(1 + self.len);
        p.rb.head.store(p.head, Ordering::Release);
        Ok(())
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
            self.head = self.rb.head.load(Ordering::Acquire);
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
        self.tail = start.wrapping_add(len);
        self.rb.tail.store(self.tail, Ordering::Release);
        Ok(len)
    }
}

impl<T: Copy, const N: usize> Drop for Consumer<'_, T, N> {
    fn drop(&mut self) {
        self.rb.consumer_locked.store(false, Ordering::Release);
    }
}
