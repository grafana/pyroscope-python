//! Walks the CPython frame chain of a thread from a signal handler and writes it into a
//! [`ringbuffer`](super::ringbuffer) record. No allocation, no locks, no recursion, no buffers.
//!
//! The handler records only code object addresses. Names are resolved later on a normal thread,
//! which must check the code object is still alive. Line numbers are not recorded.
//!
//! [`handler`] has a ready-made `SIGPROF` handler.
//!
//! Supports CPython 3.11 to 3.14, default (GIL) build, 64-bit targets.

pub mod handler;
mod layout;

pub use kindasafe_init::init;
pub use layout::{Layout, V3_11, V3_12, V3_13, V3_14};

use super::ringbuffer::{Record, RingBuffer};
use core::marker::PhantomData;
use core::sync::atomic::{AtomicUsize, Ordering};

pub const OB_TYPE: usize = 8;
const WORD: usize = size_of::<usize>();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The calling thread has no Python thread state.
    NoThreadState,
    /// Another producer holds the ring buffer.
    Contended,
    /// Memory on the chain was unreadable.
    Read,
    /// The ring buffer has no room for the stack.
    Full,
    /// The chain is longer than `max_frames`, or loops.
    TooDeep,
    /// A pointer on the chain overflows the address space.
    Invalid,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Frame {
    /// Address of the `PyCodeObject`.
    pub code: usize,
}

/// `PyGILState_GetThisThreadState`: the thread state of the calling thread, or 0.
pub type ThreadStateGetter = extern "C" fn() -> usize;

pub struct Unwinder<L> {
    code_type: usize,
    thread_state: ThreadStateGetter,
    max_frames: usize,
    metrics: Metrics,
    layout: PhantomData<L>,
}

struct Metrics {
    dropped: AtomicUsize,
    contentions: AtomicUsize,
    no_thread_state: AtomicUsize,
}

impl<L: Layout> Unwinder<L> {
    /// [`init`] must have succeeded first, or an unreadable address crashes the process.
    ///
    /// `code_type` is the address of `PyCode_Type`; frames whose executable has another type are
    /// skipped. `thread_state` returns the thread state to walk.
    /// `max_frames` bounds the walk, skipped frames included.
    pub const fn new(code_type: usize, thread_state: ThreadStateGetter, max_frames: usize) -> Self {
        Unwinder {
            code_type,
            thread_state,
            max_frames,
            metrics: Metrics {
                dropped: AtomicUsize::new(0),
                contentions: AtomicUsize::new(0),
                no_thread_state: AtomicUsize::new(0),
            },
            layout: PhantomData,
        }
    }

    /// Commits a record to `records` with a [`Frame`] for each Python frame of the calling
    /// thread, innermost first, and returns the number of frames. On error nothing is committed
    /// and the error is counted.
    pub fn unwind<const N: usize>(&self, records: &RingBuffer<Frame, N>) -> Result<usize, Error> {
        let metrics = &self.metrics;
        let tstate = (self.thread_state)();
        if tstate == 0 {
            metrics.no_thread_state.fetch_add(1, Ordering::Relaxed);
            return Err(Error::NoThreadState);
        }
        let Some(mut producer) = records.producer() else {
            metrics.contentions.fetch_add(1, Ordering::Relaxed);
            return Err(Error::Contended);
        };
        let mut record = producer.record();
        let pushed = self.walk(tstate, &mut record).inspect_err(|_| {
            metrics.dropped.fetch_add(1, Ordering::Relaxed);
        })?;
        if record.commit().is_err() {
            metrics.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(Error::Full);
        }
        Ok(pushed)
    }

    fn walk<const N: usize>(
        &self,
        tstate: usize,
        record: &mut Record<'_, '_, Frame, N>,
    ) -> Result<usize, Error> {
        let mut frame = match L::CFRAME_FRAME {
            Some(offset) => {
                let cframe = self.read_at(tstate, L::THREAD_FRAME)?;
                self.read_at(cframe, offset)?
            }
            None => self.read_at(tstate, L::THREAD_FRAME)?,
        };
        let mut visited = 0;
        let mut pushed = 0;
        while frame != 0 {
            if visited == self.max_frames {
                return Err(Error::TooDeep);
            }
            visited += 1;
            let is_python = match L::OWNER {
                Some(offset) => {
                    let owner_word = self.read_at(frame, offset & !(WORD - 1))?;
                    let owner = owner_word.to_ne_bytes()[offset & (WORD - 1)];
                    owner < 8 && L::SKIP_OWNERS & (1 << owner) == 0
                }
                None => true,
            };
            if is_python {
                let executable = self.read_at(frame, L::EXECUTABLE)? & !L::EXECUTABLE_TAG_BITS;
                if executable != 0 && self.read_at(executable, OB_TYPE)? == self.code_type {
                    record
                        .push(Frame { code: executable })
                        .map_err(|_| Error::Full)?;
                    pushed += 1;
                }
            }
            frame = self.read_at(frame, L::PREVIOUS)?;
        }
        Ok(pushed)
    }

    fn read_at(&self, base: usize, offset: usize) -> Result<usize, Error> {
        let addr = base.checked_add(offset).ok_or(Error::Invalid)?;
        kindasafe::u64(addr as u64)
            .map(|word| word as usize)
            .map_err(|_| Error::Read)
    }
}
