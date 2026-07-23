use std::marker::PhantomData;
use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};
#[cfg(target_os = "macos")]
use std::thread;

/// Runs `f` without ever parking the calling thread on a libdispatch semaphore.
///
/// On macOS, `std::thread`'s parker is backed by a `dispatch_semaphore_t`. After
/// `fork()` only the calling thread survives, but it still holds the parker it
/// created before the fork. Touching that inherited semaphore in the child (a
/// `fork()`-without-`exec()` process) makes macOS abort with a libdispatch
/// "use-after-free of dispatch_semaphore_t" SIGTRAP. See the call sites for the
/// captured stacks.
///
/// Running `f` on a freshly spawned thread gives it a parker created after the
/// fork, so any parking it does is safe. Other platforms park on a futex, which
/// is fork-safe, so `f` runs inline.
#[cfg(target_os = "macos")]
pub fn no_dispatch_semaphore<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    match thread::spawn(f).join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// See the macOS variant. Parking on non-macOS platforms uses a fork-safe
/// futex, so `f` runs directly on the calling thread.
#[cfg(not(target_os = "macos"))]
pub fn no_dispatch_semaphore<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    f()
}

/// A lazily-initialized global mutex whose contents can be abandoned
/// (leaked) and replaced with a fresh default value.
///
/// # Why it exists
///
/// This type solves a fork-safety problem. After `fork()`, only the calling
/// thread survives in the child process, but the child inherits a copy of the
/// parent's memory, including global state such as a running profiler agent
/// and any mutexes guarding it. That inherited state is unusable in the
/// child:
///
/// - a mutex held by another thread at fork time stays locked forever, so
///   any attempt to lock it deadlocks;
/// - dropping the guarded value may block on threads that don't exist in the
///   child (e.g. the agent's `stop()` joins its worker threads).
///
/// The only safe way out is to never touch the inherited value again. In an
/// `os.register_at_fork` "after in child" hook, [`leak_and_reset`] swaps in a
/// brand-new mutex around `T::default()`, deliberately leaking the old
/// allocation and everything inside it. The child then starts from a clean
/// slate, and the parent's state is never dropped or unlocked in the child.
///
/// [`leak_and_reset`]: LeakableMutex::leak_and_reset
///
/// # How to use it
///
/// ```ignore
/// static STATE: LeakableMutex<State> = LeakableMutex::new();
///
/// // Normal access from anywhere:
/// let guard = STATE.mutex().lock()?;
///
/// // In the post-fork child hook (os.register_at_fork(after_in_child=...)):
/// STATE.leak_and_reset();
/// ```
///
/// Do not cache the `&Mutex<T>` returned by [`mutex`] across a potential
/// fork: after `leak_and_reset` it points at the abandoned parent-era mutex.
/// Always re-fetch it via `STATE.mutex()` at the point of use.
///
/// [`mutex`]: LeakableMutex::mutex
pub struct LeakableMutex<T> {
    state: AtomicPtr<Mutex<T>>,
    // AtomicPtr does not inherit T's Send/Sync bounds. Model ownership of the
    // guarded value so LeakableMutex has the same auto-traits as Mutex<T>.
    _marker: PhantomData<Mutex<T>>,
}
impl<T: Default> LeakableMutex<T> {
    /// Creates an empty (uninitialized) `LeakableMutex`.
    ///
    /// `const`, so it can be used in a `static`. The inner mutex is allocated
    /// lazily on the first call to [`mutex`](LeakableMutex::mutex).
    pub const fn new() -> Self {
        Self {
            state: AtomicPtr::new(std::ptr::null_mut()),
            _marker: PhantomData,
        }
    }

    /// Returns the current inner mutex, allocating `Mutex::new(T::default())`
    /// on first use.
    ///
    /// Call this at every point of use instead of caching the returned
    /// reference, so that after [`leak_and_reset`](LeakableMutex::leak_and_reset)
    /// you observe the fresh mutex rather than the abandoned one.
    pub fn mutex(&self) -> &Mutex<T> {
        unsafe {
            let cur = self.state.load(Ordering::SeqCst);
            if !cur.is_null() {
                return &*cur;
            }

            let new = Self::new_static();
            let res = self.state.compare_exchange(
                std::ptr::null_mut(),
                new,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            match res {
                Ok(_) => &*new,
                Err(old) => {
                    drop(Box::from_raw(new));
                    &*old
                }
            }
        }
    }

    /// Abandons the current mutex and its contents, replacing them with a
    /// fresh `Mutex::new(T::default())`.
    ///
    /// The old allocation is intentionally leaked: neither the mutex nor the
    /// `T` inside it is dropped. This is the whole point — after `fork()` the
    /// child must never unlock or drop state inherited from the parent.
    ///
    /// Intended to be called only from a post-fork child hook while the
    /// process is effectively single-threaded. Callers racing with this from
    /// other threads may still hold references to the old mutex, which stays
    /// valid (it is leaked, not freed), but their updates will be lost.
    pub fn leak_and_reset(&self) {
        self.state.store(Self::new_static(), Ordering::SeqCst)
    }

    fn new_static() -> *mut Mutex<T> {
        Box::into_raw(Box::new(Mutex::new(T::default())))
    }
}

#[cfg(test)]
mod tests {
    use super::LeakableMutex;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn initializes_lazily_once() {
        let state = LeakableMutex::<usize>::new();

        assert_eq!(*state.mutex().lock().unwrap(), 0);
        *state.mutex().lock().unwrap() = 42;
        assert_eq!(*state.mutex().lock().unwrap(), 42);
    }

    #[test]
    fn concurrent_initialization_uses_one_mutex() {
        const THREADS: usize = 4;

        let state = Arc::new(LeakableMutex::<usize>::new());
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);

        for _ in 0..THREADS {
            let state = Arc::clone(&state);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                *state.mutex().lock().unwrap() += 1;
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(*state.mutex().lock().unwrap(), THREADS);
    }

    #[test]
    fn reset_preserves_old_reference_and_uses_fresh_default() {
        let state = LeakableMutex::<usize>::new();
        let old = state.mutex();
        *old.lock().unwrap() = 42;

        state.leak_and_reset();

        let new = state.mutex();
        assert!(!std::ptr::eq(old, new));
        assert_eq!(*new.lock().unwrap(), 0);
        assert_eq!(*old.lock().unwrap(), 42);
    }

    #[test]
    fn is_send_and_sync_for_send_values() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<LeakableMutex<usize>>();
    }
}
