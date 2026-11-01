//! Fault-tolerant byte observations of this process's address space.
//!
//! Sources may be foreign allocations or stale/unmapped addresses: no Rust
//! reference is created from a source address. Catching an access fault does
//! not establish object identity, lifetime, initialization, or synchronization.
//! In particular, callers must synchronize CPython object access (including
//! allocation/reuse) as required by CPython; recovery is not a replacement for
//! the GIL or other ownership protocols. A successful read may still contain
//! semantically stale data and must be validated by the consumer.
//!
//! Supported targets: Linux/macOS on x86_64/aarch64.
//!
//! Architecture primitives are deliberately inaccessible to consumers:
//! ```compile_fail
//! pykindasafe::arch::slice(core::ptr::null_mut(), 0, 1);
//! ```

mod recovery;

pub use recovery::{InitError, init, is_initialized, sanity_check};

#[derive(Debug, PartialEq)]
pub struct ReadMemError {
    pub signal: u64,
}

pub type Ptr = u64;

/// Observe eight bytes, allowing an unaligned source.
///
/// # Safety
/// For any bytes actually read, the caller must have permission to observe
/// initialized memory without racing non-atomic writes or violating the
/// exclusivity of a live reference. The address need not identify a live Rust
/// allocation: inaccessible/stale foreign addresses are permitted, but readable
/// reused memory must still satisfy these observation obligations. Do not read
/// device memory or locations where a byte load has side effects.
///
/// `init` must have succeeded, its handlers must still be installed, and both
/// SIGSEGV and SIGBUS must be deliverable on this thread, including inside a
/// nested signal handler. The caller must preserve those conditions throughout
/// the read. A fault is reported; it does not prove other source accesses safe.
///
/// ```compile_fail
/// let _ = pykindasafe::u64(0);
/// ```
pub unsafe fn u64(at: Ptr) -> Result<Ptr, ReadMemError> {
    let res = unsafe { arch::u64(at) };
    if res.signal == 0 {
        Ok(res.value)
    } else {
        Err(ReadMemError { signal: res.signal })
    }
}

// Keep the writable derivation shared by both public copy wrappers. This small
// boundary also lets Miri check destination provenance without executing asm.
fn with_destination<T>(buf: &mut [u8], copy: impl FnOnce(*mut u8, u64) -> T) -> T {
    copy(buf.as_mut_ptr(), buf.len() as u64)
}

/// Copy observations into `buf`; an error may leave a partially copied buffer.
///
/// # Safety
/// The source range of `buf.len()` bytes must satisfy the observation and
/// recovery obligations of [`u64`]. Source and destination must not overlap;
/// `buf` must remain exclusively writable throughout the operation. No source
/// alignment or Rust allocation membership is required. Source address plus
/// length must not wrap the address space. Until the assembly count is widened,
/// the length must also fit in a `u32` on x86_64.
///
/// ```compile_fail
/// let _ = pykindasafe::slice(&mut [0; 8], 0);
/// ```
pub unsafe fn slice(buf: &mut [u8], at: Ptr) -> Result<(), ReadMemError> {
    let res = with_destination(buf, |dst, len| unsafe { arch::slice(dst, at, len) });
    if res.signal == 0 {
        Ok(())
    } else {
        Err(ReadMemError { signal: res.signal })
    }
}

/// Copy `buf.len()` bytes and return the UTF-8 prefix before the first NUL.
/// A null source returns the empty string. Errors may leave `buf` modified.
///
/// # Safety
/// For non-null sources, all [`slice`] obligations apply to the full buffer
/// length, including bytes following a possible NUL. The returned string borrows
/// only `buf`; no reference into the foreign source is returned.
///
/// ```compile_fail
/// let _ = pykindasafe::str(&mut [0; 8], 1);
/// ```
pub unsafe fn str(buf: &mut [u8], at: Ptr) -> Result<&str, ReadMemError> {
    if at == 0 {
        return Ok("");
    }
    let res = with_destination(buf, |dst, len| unsafe { arch::slice(dst, at, len) });
    if res.signal != 0 {
        return Err(ReadMemError { signal: res.signal });
    }
    for i in 0..buf.len() {
        if buf[i] == 0 {
            let v = &buf[..i];
            return match core::str::from_utf8(v) {
                Ok(v) => Ok(v),
                Err(_) => Err(ReadMemError { signal: 228 }), //todo
            };
        }
    }
    Err(ReadMemError { signal: 229 }) //todo
}

pub(crate) fn crash_points() -> CrashPoints {
    arch::crash_points()
}

#[derive(Copy, Clone)]
pub(crate) struct CrashPoint {
    pub pc: usize,
    pub signal_reg: Reg,
    pub skip: usize,
}
const CRASH_POINTS_COUNT: usize = 2;

#[derive(Copy, Clone)]
pub(crate) struct CrashPoints {
    pub crash_points: [CrashPoint; CRASH_POINTS_COUNT],
}

#[cfg(target_arch = "x86_64")]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Reg {
    Rax,
    Rdx,
}

#[cfg(target_arch = "aarch64")]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum Reg {
    X0,
    X1,
}

#[cfg(target_arch = "x86_64")]
mod arch {

    #[repr(C)]
    pub struct U64Res {
        pub value: u64,
        pub signal: u64,
    }

    #[unsafe(naked)]
    pub unsafe extern "sysv64" fn u64(_at: u64) -> U64Res {
        core::arch::naked_asm!(
            "mov rax, [rdi]", // 00010000 	48 8B 07 	mov 	rax, qword ptr [rdi]
            "xor edx, edx",   // 00010003 	31 D2 	xor 	edx, edx
            "ret",            // 00010005 	C3 	ret
        )
    }

    #[repr(C)]
    pub struct VecResult {
        pub signal: u64,
    }

    #[unsafe(naked)]
    pub unsafe extern "sysv64" fn slice(
        _dst: *mut u8, // rdi
        _src: u64,     // rsi
        _n: u64,       // rdx
    ) -> VecResult {
        core::arch::naked_asm!(
            "mov ecx, edx", // 00010000 	89 D1 	mov 	ecx, edx
            "rep movsb",    // 00010002 	F3 A4 	rep movsb 	byte ptr [rdi], byte ptr [rsi]
            "xor eax, eax", // 00010004 	31 C0 	xor 	eax, eax
            "ret",          // 00010006 	C3 	ret
        )
    }

    pub(crate) fn crash_points() -> crate::CrashPoints {
        crate::CrashPoints {
            crash_points: [
                crate::CrashPoint {
                    pc: u64 as *const () as usize,
                    signal_reg: crate::Reg::Rdx,
                    skip: 5,
                },
                crate::CrashPoint {
                    pc: slice as *const () as usize + 2, // +2 for 89 D1 	mov 	ecx, edx
                    signal_reg: crate::Reg::Rax,
                    skip: 4,
                },
            ],
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {

    #[repr(C)]
    pub struct U64Res {
        pub value: u64,
        pub signal: u64,
    }

    #[unsafe(naked)]
    pub unsafe extern "C" fn u64(_at: u64) -> U64Res {
        core::arch::naked_asm!(
            "ldr x0, [x0]", // offset 0: load 64-bit value from address in x0
            "mov x1, #0",   // offset 4: signal = 0 (success)
            "ret",          // offset 8
        )
    }

    #[repr(C)]
    pub struct VecResult {
        pub signal: u64,
    }

    #[unsafe(naked)]
    pub unsafe extern "C" fn slice(
        _dst: *mut u8, // x0
        _src: u64,     // x1
        _n: u64,       // x2
    ) -> VecResult {
        core::arch::naked_asm!(
            "cbz x2, 2f", // offset 0: skip if n==0
            "1:",
            "ldrb w3, [x1], #1", // offset 4: load byte from src, post-increment
            "strb w3, [x0], #1", // offset 8: store byte to dst, post-increment
            "subs x2, x2, #1",   // offset 12: decrement counter
            "b.ne 1b",           // offset 16: loop if not zero
            "2:",
            "mov x0, #0", // offset 20: signal = 0 (success)
            "ret",        // offset 24
        )
    }

    pub(crate) fn crash_points() -> crate::CrashPoints {
        crate::CrashPoints {
            crash_points: [
                crate::CrashPoint {
                    pc: u64 as *const () as usize,
                    signal_reg: crate::Reg::X1,
                    skip: 8, // skip ldr + mov to land on ret
                },
                crate::CrashPoint {
                    pc: slice as *const () as usize + 4, // +4 for cbz
                    signal_reg: crate::Reg::X0,
                    skip: 20, // skip ldrb + strb + subs + b.ne + mov to land on ret
                },
            ],
        }
    }
}

#[cfg(test)]
mod pointer_contract_tests {
    use super::*;

    #[test]
    fn mutable_destination_is_writable() {
        let mut destination = [0; 8];
        with_destination(&mut destination, |ptr, len| {
            // SAFETY: the callback receives exclusive access to the entire buffer.
            unsafe { core::ptr::write_bytes(ptr, 0x61, len as usize) };
        });
        assert_eq!(destination, [0x61; 8]);
    }

    #[test]
    #[cfg(not(miri))]
    fn both_wrappers_copy_into_mutable_storage() {
        unsafe { init() }.unwrap();
        let source = *b"hello\0";
        let mut destination = [0; 6];
        // SAFETY: initialized, stable non-overlapping arrays and active recovery.
        unsafe { slice(&mut destination, source.as_ptr() as Ptr) }.unwrap();
        assert_eq!(destination, source);
        destination.fill(0xff);
        assert_eq!(
            unsafe { str(&mut destination, source.as_ptr() as Ptr) },
            Ok("hello")
        );
        assert_eq!(destination, source);
    }
}
