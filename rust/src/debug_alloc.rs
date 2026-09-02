//! Guarding global allocator, used only by the reproducer builds for
//! grafana/pyroscope-python#37.
//!
//! Every Rust allocation gets a 16-byte header holding a magic word and the
//! requested size. On deallocation the header is validated and then poisoned,
//! so the *first* bad operation aborts with a backtrace instead of silently
//! corrupting the heap and crashing somewhere unrelated later:
//!
//!   * freeing a pointer this allocator never handed out (e.g. a pointer read
//!     out of the profiled interpreter) -> bad magic,
//!   * freeing the same pointer twice -> poisoned magic,
//!   * freeing with a Layout that does not match the allocation -> size/align
//!     mismatch,
//!   * a write that runs past the end of an allocation -> the next chunk's
//!     magic is destroyed and reported when that chunk is freed.
//!
//! CPython's own allocations are not affected: they go to libc malloc directly.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, Ordering};

const MAGIC_LIVE: u64 = 0x5052_4F53_434F_5045; // "PROSCOPE"
const MAGIC_DEAD: u64 = 0xDEAD_5052_4F53_0000;
const HEADER: usize = 16;

/// Bytes reserved in front of the caller's pointer. The magic/size words sit
/// in the last 16 bytes of it, so an over-aligned allocation keeps its
/// alignment.
#[inline]
fn offset(layout: Layout) -> usize {
    layout.align().max(HEADER)
}

pub struct GuardAlloc;

static REPORTING: AtomicBool = AtomicBool::new(false);

#[inline]
fn header_layout(layout: Layout) -> Layout {
    Layout::from_size_align(layout.size() + offset(layout), offset(layout)).unwrap()
}

#[inline]
unsafe fn write_header(base: *mut u8, layout: Layout) -> *mut u8 {
    unsafe {
        let user = base.add(offset(layout));
        (user.sub(16) as *mut u64).write(MAGIC_LIVE);
        (user.sub(8) as *mut u64).write(layout.size() as u64);
        user
    }
}

fn report(what: &str, ptr: *mut u8, magic: u64, stored_size: u64, layout: Layout) -> ! {
    // Guard against recursion: printing allocates.
    if !REPORTING.swap(true, Ordering::SeqCst) {
        eprintln!(
            "\n=== pyroscope guard allocator: {what} ===\n\
             ptr={ptr:p} magic=0x{magic:016x} stored_size={stored_size} \
             dealloc_size={} dealloc_align={}\n{}",
            layout.size(),
            layout.align(),
            std::backtrace::Backtrace::force_capture()
        );
    }
    std::process::abort()
}

unsafe impl GlobalAlloc for GuardAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = unsafe { System.alloc(header_layout(layout)) };
        if base.is_null() {
            return base;
        }
        unsafe { write_header(base, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let base = unsafe { System.alloc_zeroed(header_layout(layout)) };
        if base.is_null() {
            return base;
        }
        unsafe { write_header(base, layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let base = unsafe { ptr.sub(offset(layout)) };
        let magic = unsafe { (ptr.sub(16) as *const u64).read() };
        let stored = unsafe { (ptr.sub(8) as *const u64).read() };
        if magic != MAGIC_LIVE {
            let what = if magic == MAGIC_DEAD {
                "double free"
            } else {
                "free of a pointer this allocator never returned"
            };
            report(what, ptr, magic, stored, layout);
        }
        if stored != layout.size() as u64 {
            report("size mismatch on free", ptr, magic, stored, layout);
        }
        unsafe {
            (ptr.sub(16) as *mut u64).write(MAGIC_DEAD);
            System.dealloc(base, header_layout(layout));
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let base = unsafe { ptr.sub(offset(layout)) };
        let magic = unsafe { (ptr.sub(16) as *const u64).read() };
        let stored = unsafe { (ptr.sub(8) as *const u64).read() };
        if magic != MAGIC_LIVE {
            let what = if magic == MAGIC_DEAD {
                "realloc of a freed pointer"
            } else {
                "realloc of a pointer this allocator never returned"
            };
            report(what, ptr, magic, stored, layout);
        }
        if stored != layout.size() as u64 {
            report("size mismatch on realloc", ptr, magic, stored, layout);
        }
        let new_layout = Layout::from_size_align(new_size, layout.align()).unwrap();
        let new_base = unsafe {
            System.realloc(base, header_layout(layout), new_size + offset(new_layout))
        };
        if new_base.is_null() {
            return new_base;
        }
        unsafe { write_header(new_base, new_layout) }
    }
}
