//! CPython allocator hooks.
//!
//! A transliteration of `cpp/_memalloc.cpp`, which was the last of the C++.
//! These are `extern "C"` function pointers that CPython calls for every
//! object allocation in the process, so the constraints are severe:
//!
//! * **Must not unwind.** A panic escaping a non-`-unwind` `extern "C"` fn is
//!   not undefined behaviour as of Rust 1.81, but rustc emits a shim that
//!   aborts, so a profiler bug becomes a hard crash of the host process.
//!   Nothing here can panic, which the module-wide lint wall in
//!   [`crate::memalloc`] enforces. (`core::panic::abort_unwind` would state
//!   that intent at the boundary, but it is still unstable on the pinned
//!   toolchain.)
//! * **Must not allocate through PyMem**, which would re-enter. See the
//!   module docs in [`crate::memalloc`].
//! * **Must not log.** Formatting allocates, and a logger is arbitrary code.
//!
//! Only `PYMEM_DOMAIN_OBJ` and, optionally, `PYMEM_DOMAIN_MEM` are hooked.
//! `PYMEM_DOMAIN_RAW` deliberately is not: that is what lets everything here
//! assume the GIL is held.

use crate::memalloc::runtime::heap;
use pyo3::ffi::{PyMem_GetAllocator, PyMem_SetAllocator, PyMemAllocatorDomain, PyMemAllocatorEx};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// Published pointer to the allocator we displaced, per domain.
///
/// # Why a leaked `Box` per `start()`
///
/// The C++ used a two-slot array plus an index, writing to whichever slot
/// in-flight hooks from the previous cycle were not reading. That reasoning
/// holds for exactly one concurrent cycle; its own comment had to argue about
/// slot parity. Leaking one allocation per `start()` makes *every* historical
/// cycle's pointer valid forever, so an in-flight hook from any cycle is safe
/// and there is no parity argument to get wrong. `start()` happens once per
/// process, so the cost is a few dozen bytes.
struct SavedAllocator {
    published: AtomicPtr<PyMemAllocatorEx>,
    installed: AtomicBool,
}

impl SavedAllocator {
    const fn new() -> Self {
        Self {
            published: AtomicPtr::new(std::ptr::null_mut()),
            installed: AtomicBool::new(false),
        }
    }

    /// Publish the displaced allocator so the hooks can delegate to it.
    fn publish(&self, saved: PyMemAllocatorEx) {
        let leaked = Box::into_raw(Box::new(saved));
        // Release, so the struct's contents are visible to any hook that
        // subsequently loads the pointer with Acquire.
        self.published.store(leaked, Ordering::Release);
        self.installed.store(true, Ordering::Release);
    }

    /// Read the displaced allocator, or `None` if nothing is published.
    #[inline]
    fn load(&self) -> Option<PyMemAllocatorEx> {
        let published = self.published.load(Ordering::Acquire);
        if published.is_null() {
            return None;
        }
        // SAFETY: the pointer came from `Box::into_raw` in `publish` and is
        // never freed, precisely so that a hook from any cycle can read it.
        //
        // `read` rather than `&*`: a byte copy creates no reference that could
        // alias a concurrent initialising write, mirroring the C++
        // `PyMemAllocatorEx alloc = *saved;`.
        Some(unsafe { published.read() })
    }
}

/// Displaced `PYMEM_DOMAIN_OBJ` allocator.
static SAVED_OBJ: SavedAllocator = SavedAllocator::new();
/// Displaced `PYMEM_DOMAIN_MEM` allocator.
static SAVED_MEM: SavedAllocator = SavedAllocator::new();

/// Shared body of `malloc` and `calloc`.
///
/// `use_calloc` picks which underlying entry point to delegate to, matching
/// the C++ `memalloc_alloc`.
#[inline]
fn hook_alloc(
    saved: &SavedAllocator,
    use_calloc: bool,
    nelem: usize,
    elsize: usize,
) -> *mut c_void {
    // A null guard on every load, matching realloc and free: leaking is safer
    // than crashing through a torn pointer.
    let Some(alloc) = saved.load() else {
        return std::ptr::null_mut();
    };

    // The C++ let `nelem * elsize` wrap. Saturating instead: if it would wrap,
    // the underlying calloc has already failed and returned null, so the
    // tracked size never matters, but it must not be a wrapped value either.
    let size = nelem.saturating_mul(elsize);

    let ptr = if use_calloc {
        let Some(calloc) = alloc.calloc else {
            return std::ptr::null_mut();
        };
        // Delegating to the allocator we displaced, with its own ctx.
        calloc(alloc.ctx, nelem, elsize)
    } else {
        let Some(malloc) = alloc.malloc else {
            return std::ptr::null_mut();
        };
        malloc(alloc.ctx, size)
    };

    if !ptr.is_null() {
        heap::heap_track(ptr, size);
    }
    ptr
}

/// Shared body of `realloc`.
#[inline]
fn hook_realloc(saved: &SavedAllocator, ptr: *mut c_void, new_size: usize) -> *mut c_void {
    let Some(alloc) = saved.load() else {
        return std::ptr::null_mut();
    };
    let Some(realloc) = alloc.realloc else {
        return std::ptr::null_mut();
    };
    // Delegating to the allocator we displaced, with its own ctx.
    let new_ptr = realloc(alloc.ctx, ptr, new_size);

    if !new_ptr.is_null() {
        heap::heap_untrack(ptr);
        heap::heap_track(new_ptr, new_size);
    } else if new_size == 0 && !ptr.is_null() {
        // realloc(ptr, 0) is implementation-defined: some allocators (glibc
        // among them) free ptr and return null. In that case ptr is gone and
        // must be untracked, or the live map keeps a stale entry forever.
        //
        // When new_size > 0 and the result is null the allocation merely
        // failed: ptr is still valid and must stay tracked. Hence the
        // new_size == 0 condition.
        heap::heap_untrack(ptr);
    }
    new_ptr
}

/// Shared body of `free`.
#[inline]
fn hook_free(saved: &SavedAllocator, ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let Some(alloc) = saved.load() else {
        return;
    };
    let Some(free) = alloc.free else {
        return;
    };
    heap::heap_untrack(ptr);
    // Delegating to the allocator we displaced, with its own ctx.
    free(alloc.ctx, ptr);
}

// The OBJ and MEM domains get separate hook functions reading separate saved
// allocators, rather than one parameterised set, so each domain's lifecycle
// stays fully independent -- as in the C++.

extern "C" fn obj_malloc(_ctx: *mut c_void, size: usize) -> *mut c_void {
    hook_alloc(&SAVED_OBJ, false, 1, size)
}

extern "C" fn obj_calloc(_ctx: *mut c_void, nelem: usize, elsize: usize) -> *mut c_void {
    hook_alloc(&SAVED_OBJ, true, nelem, elsize)
}

extern "C" fn obj_realloc(_ctx: *mut c_void, ptr: *mut c_void, new_size: usize) -> *mut c_void {
    hook_realloc(&SAVED_OBJ, ptr, new_size)
}

extern "C" fn obj_free(_ctx: *mut c_void, ptr: *mut c_void) {
    hook_free(&SAVED_OBJ, ptr);
}

extern "C" fn mem_malloc(_ctx: *mut c_void, size: usize) -> *mut c_void {
    hook_alloc(&SAVED_MEM, false, 1, size)
}

extern "C" fn mem_calloc(_ctx: *mut c_void, nelem: usize, elsize: usize) -> *mut c_void {
    hook_alloc(&SAVED_MEM, true, nelem, elsize)
}

extern "C" fn mem_realloc(_ctx: *mut c_void, ptr: *mut c_void, new_size: usize) -> *mut c_void {
    hook_realloc(&SAVED_MEM, ptr, new_size)
}

extern "C" fn mem_free(_ctx: *mut c_void, ptr: *mut c_void) {
    hook_free(&SAVED_MEM, ptr);
}

fn obj_hooks() -> PyMemAllocatorEx {
    PyMemAllocatorEx {
        ctx: std::ptr::null_mut(),
        malloc: Some(obj_malloc),
        calloc: Some(obj_calloc),
        realloc: Some(obj_realloc),
        free: Some(obj_free),
    }
}

fn mem_hooks() -> PyMemAllocatorEx {
    PyMemAllocatorEx {
        ctx: std::ptr::null_mut(),
        malloc: Some(mem_malloc),
        calloc: Some(mem_calloc),
        realloc: Some(mem_realloc),
        free: Some(mem_free),
    }
}

/// Install our hooks on one domain, publishing the allocator we displace.
///
/// # Safety
///
/// Must be called with the GIL held.
unsafe fn install(
    domain: PyMemAllocatorDomain,
    saved: &SavedAllocator,
    mut hooks: PyMemAllocatorEx,
) {
    let mut previous = PyMemAllocatorEx {
        ctx: std::ptr::null_mut(),
        malloc: None,
        calloc: None,
        realloc: None,
        free: None,
    };
    // SAFETY: the caller holds the GIL; both calls are the documented API for
    // swapping an allocator.
    unsafe {
        PyMem_GetAllocator(domain, &raw mut previous);
    }
    saved.publish(previous);
    // SAFETY: as above. Published before installing, so the very first hook
    // call already has something to delegate to.
    unsafe {
        PyMem_SetAllocator(domain, &raw mut hooks);
    }
}

/// Restore the allocator we displaced on one domain.
///
/// # Safety
///
/// Must be called with the GIL held.
unsafe fn restore(domain: PyMemAllocatorDomain, saved: &SavedAllocator) {
    let Some(mut previous) = saved.load() else {
        return;
    };
    // SAFETY: the caller holds the GIL.
    unsafe {
        PyMem_SetAllocator(domain, &raw mut previous);
    }
    saved.installed.store(false, Ordering::Release);

    // Deliberately leave the published pointer in place.
    //
    // Once PyMem_SetAllocator has restored the real allocator CPython no
    // longer dispatches to our hooks, so a stale-but-valid pointer is
    // harmless. Clearing it would make an already-dispatched free hook load
    // null and return *without delegating to the underlying free*, leaking
    // that block while CPython believes it was freed. The C++ called this out
    // explicitly for the same reason; it reads like an oversight and is not.
}

/// Install the allocator hooks.
///
/// # Safety
///
/// Must be called with the GIL held, and only when not already installed.
pub unsafe fn install_hooks(enable_mem_domain: bool) {
    // SAFETY: the caller holds the GIL.
    unsafe {
        install(
            PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ,
            &SAVED_OBJ,
            obj_hooks(),
        );
    }
    if enable_mem_domain {
        // On 3.13+ the MEM domain is unconditionally available, so the
        // version guard the C++ needed is gone.
        // SAFETY: as above.
        unsafe {
            install(
                PyMemAllocatorDomain::PYMEM_DOMAIN_MEM,
                &SAVED_MEM,
                mem_hooks(),
            );
        }
    }
}

/// Uninstall the allocator hooks.
///
/// # Safety
///
/// Must be called with the GIL held.
pub unsafe fn uninstall_hooks() {
    // SAFETY: the caller holds the GIL.
    unsafe {
        restore(PyMemAllocatorDomain::PYMEM_DOMAIN_OBJ, &SAVED_OBJ);
    }
    if SAVED_MEM.installed.load(Ordering::Acquire) {
        // SAFETY: as above.
        unsafe {
            restore(PyMemAllocatorDomain::PYMEM_DOMAIN_MEM, &SAVED_MEM);
        }
    }
}
