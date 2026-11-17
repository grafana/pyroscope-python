use core::sync::atomic::{AtomicUsize, Ordering};
use pyo3::Python;

pub(super) fn register_thread(py: Python<'_>, id: u64) -> Result<bool, libc::c_int> {
    if !is_current_thread(id) {
        return Ok(false);
    }
    register_current_thread(py)?;
    Ok(true)
}

pub(super) fn unregister_thread(py: Python<'_>, id: u64) -> Result<(), libc::c_int> {
    if !is_current_thread(id) {
        log::error!(target: super::LOG_TAG, "cannot unregister Python thread {id}: not the current thread");
        return Ok(());
    }
    unregister_current_thread(py)
}

fn is_current_thread(id: u64) -> bool {
    id == unsafe { pyo3::ffi::PyThread_get_thread_ident() }
}

const UNINITIALIZED: usize = usize::MAX;
static KEY: AtomicUsize = AtomicUsize::new(UNINITIALIZED);

fn get_or_create_thread_state_key() -> Result<libc::pthread_key_t, libc::c_int> {
    let key = KEY.load(Ordering::Acquire);
    if key != UNINITIALIZED {
        return Ok(key as libc::pthread_key_t);
    }
    let mut key = 0;
    // SAFETY: key is writable and no destructor is needed for a borrowed pointer.
    let error = unsafe { libc::pthread_key_create(&mut key, None) };
    if error != 0 {
        return Err(error);
    }
    match KEY.compare_exchange(
        UNINITIALIZED,
        key as usize,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(key),
        Err(published) => {
            // SAFETY: this unused key was never published or populated.
            unsafe { libc::pthread_key_delete(key) };
            Ok(published as libc::pthread_key_t)
        }
    }
}

pub(super) fn register_current_thread(_py: Python<'_>) -> Result<(), libc::c_int> {
    let key = get_or_create_thread_state_key()?;
    // SAFETY: the Python token proves that this thread has an attached state.
    let tstate = unsafe { pyo3::ffi::PyThreadState_Get() };
    // SAFETY: key is valid for the process lifetime. This runs outside a signal handler.
    let error = unsafe { libc::pthread_setspecific(key, tstate.cast()) };
    if error != 0 {
        return Err(error);
    }
    Ok(())
}

pub(super) fn unregister_current_thread(_py: Python<'_>) -> Result<(), libc::c_int> {
    let key = KEY.load(Ordering::Acquire);
    if key != UNINITIALIZED {
        // SAFETY: published keys remain valid for the process lifetime.
        let error = unsafe { libc::pthread_setspecific(key as _, core::ptr::null()) };
        if error != 0 {
            return Err(error);
        }
    }
    Ok(())
}

#[inline(always)]
pub(super) fn current_thread_state() -> usize {
    let key = KEY.load(Ordering::Acquire);
    if key == UNINITIALIZED {
        return 0;
    }
    // SAFETY: the key is never deleted. On supported Linux/macOS libcs, getspecific
    // is a read-only TLS lookup without allocation or locks. Do not call CPython's
    // 3.15 lookup here: its compiler TLS access can enter the dynamic loader.
    unsafe { libc::pthread_getspecific(key as _) as usize }
}
