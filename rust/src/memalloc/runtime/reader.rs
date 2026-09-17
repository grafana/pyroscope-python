//! Reading this process's own memory for the frame walk.
//!
//! The production [`Mem`] implementation. Every read is a plain load from an
//! address CPython gave us, with no bounds checking available, so correctness
//! rests on two things: the offsets having been validated at start-up (see
//! [`crate::memalloc::pure::offsets`]), and the GIL being held, which keeps
//! the objects being walked alive.

use crate::memalloc::pure::frames::Mem;

/// Reads the current process directly.
///
/// Zero-sized: it carries no state, only the invariant documented below.
pub struct InProcess;

impl InProcess {
    /// Is `addr` plausibly a readable object pointer?
    ///
    /// Only rules out the obviously impossible. A null or tiny address means
    /// we followed a field that was not what we thought; anything else we have
    /// to trust, because there is no way to probe readability without a
    /// syscall per access.
    #[inline]
    fn plausible(addr: usize) -> bool {
        // The first page is never mapped on any platform we support.
        addr >= 4096
    }
}

// SAFETY: the caller (the allocator hook) holds the GIL, so the objects being
// walked cannot be freed underneath us, and the offsets were validated against
// this interpreter's own `_Py_DebugOffsets` before any hook was installed.
// Reads of implausible addresses are refused rather than attempted.
unsafe impl Mem for InProcess {
    #[inline]
    fn read_usize(&self, addr: usize) -> Option<usize> {
        if !Self::plausible(addr) {
            return None;
        }
        // SAFETY: see the impl-level comment. `read_unaligned` because
        // nothing guarantees CPython's fields are aligned for our load width
        // once an offset is applied.
        Some(unsafe { (addr as *const usize).read_unaligned() })
    }

    #[inline]
    fn read_u8(&self, addr: usize) -> Option<u8> {
        if !Self::plausible(addr) {
            return None;
        }
        // SAFETY: see the impl-level comment.
        Some(unsafe { (addr as *const u8).read() })
    }

    #[inline]
    fn read_i32(&self, addr: usize) -> Option<i32> {
        if !Self::plausible(addr) {
            return None;
        }
        // SAFETY: see the impl-level comment.
        Some(unsafe { (addr as *const i32).read_unaligned() })
    }

    #[inline]
    fn read_bytes(&self, addr: usize, len: usize) -> Option<&[u8]> {
        if !Self::plausible(addr) {
            return None;
        }
        if len == 0 {
            return Some(&[]);
        }
        // Refuse a length that would wrap the address space rather than
        // construct a slice that cannot exist.
        addr.checked_add(len)?;
        // SAFETY: see the impl-level comment. The caller has checked `len`
        // against a sane bound, and the bytes belong to a live `bytes` or
        // `str` object which cannot be freed while we hold the GIL.
        Some(unsafe { std::slice::from_raw_parts(addr as *const u8, len) })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]

    use super::*;

    #[test]
    fn reads_real_memory() {
        let value: usize = 0x1234_5678_9abc_def0;
        let addr = (&raw const value) as usize;
        assert_eq!(InProcess.read_usize(addr), Some(value));
    }

    #[test]
    fn reads_bytes_back() {
        let data = b"hello frame walker";
        let addr = data.as_ptr() as usize;
        assert_eq!(InProcess.read_bytes(addr, data.len()), Some(&data[..]));
    }

    #[test]
    fn a_zero_length_read_is_an_empty_slice() {
        let data = b"x";
        let addr = data.as_ptr() as usize;
        assert_eq!(InProcess.read_bytes(addr, 0), Some(&[][..]));
    }

    /// Null and near-null addresses are the ones a mis-followed field is most
    /// likely to produce, and must be refused rather than dereferenced.
    #[test]
    fn implausible_addresses_are_refused() {
        for addr in [0usize, 1, 8, 4095] {
            assert_eq!(InProcess.read_usize(addr), None, "addr {addr}");
            assert_eq!(InProcess.read_u8(addr), None, "addr {addr}");
            assert_eq!(InProcess.read_i32(addr), None, "addr {addr}");
            assert_eq!(InProcess.read_bytes(addr, 8), None, "addr {addr}");
        }
    }

    #[test]
    fn a_length_that_would_wrap_is_refused() {
        let data = b"x";
        let addr = data.as_ptr() as usize;
        assert_eq!(InProcess.read_bytes(addr, usize::MAX), None);
    }
}
