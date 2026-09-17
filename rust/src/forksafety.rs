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
/// The accessors take `&'static self`, so the type is only usable from a
/// `static` (or another never-dropped location such as a leaked allocation);
/// non-static usage does not compile.
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
    ///
    /// Takes `&'static self`: this type is only meant to live in a `static`
    /// (its leak-instead-of-drop contract assumes the instance itself is
    /// never dropped), so non-static usage is rejected at compile time.
    pub fn mutex(&'static self) -> &'static Mutex<T> {
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
    ///
    /// Takes `&'static self` for the same reason as
    /// [`mutex`](LeakableMutex::mutex).
    #[cfg(not(miri))]
    pub fn leak_and_reset(&'static self) {
        self.leak_and_reset_impl();
    }

    /// Miri-only variant of `leak_and_reset` (see the `#[cfg(not(miri))]`
    /// item for the full contract). It additionally returns the abandoned
    /// allocation so tests can reclaim it and keep Miri's leak checker happy,
    /// or `None` if the mutex was never initialized.
    #[cfg(miri)]
    #[must_use = "reclaim the returned allocation or Miri reports a leak"]
    pub fn leak_and_reset(&'static self) -> Option<std::ptr::NonNull<Mutex<T>>> {
        std::ptr::NonNull::new(self.leak_and_reset_impl())
    }

    fn leak_and_reset_impl(&self) -> *mut Mutex<T> {
        self.state.swap(Self::new_static(), Ordering::SeqCst)
    }

    fn new_static() -> *mut Mutex<T> {
        Box::into_raw(Box::new(Mutex::new(T::default())))
    }
}

// No `Drop` impl on purpose: `mutex` takes `&'static self`, so an instance
// that could be dropped can never have allocated its inner mutex — there is
// nothing to free. Dropping inherited state is the exact hazard this type
// exists to avoid, so leaking on drop is the correct default anyway.

// Each test declares its own `static` because the accessors take
// `&'static self`. The mutex still reachable through a static at process
// exit is not a leak for Miri (its leak checker is reachability-based);
// only allocations abandoned by `leak_and_reset` need reclaiming.
#[cfg(test)]
mod tests {
    use super::LeakableMutex;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn initializes_lazily_once() {
        static STATE: LeakableMutex<usize> = LeakableMutex::new();

        assert_eq!(*STATE.mutex().lock().unwrap(), 0);
        *STATE.mutex().lock().unwrap() = 42;
        assert_eq!(*STATE.mutex().lock().unwrap(), 42);
    }

    #[test]
    fn concurrent_initialization_uses_one_mutex() {
        const THREADS: usize = 4;
        static STATE: LeakableMutex<usize> = LeakableMutex::new();

        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::with_capacity(THREADS);

        for _ in 0..THREADS {
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                *STATE.mutex().lock().unwrap() += 1;
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(*STATE.mutex().lock().unwrap(), THREADS);
    }

    #[test]
    fn reset_preserves_old_reference_and_uses_fresh_default() {
        static STATE: LeakableMutex<usize> = LeakableMutex::new();

        let old = STATE.mutex();
        *old.lock().unwrap() = 42;

        #[cfg(miri)]
        let leaked = STATE.leak_and_reset();
        #[cfg(not(miri))]
        STATE.leak_and_reset();

        let new = STATE.mutex();
        assert!(!std::ptr::eq(old, new));
        assert_eq!(*new.lock().unwrap(), 0);
        assert_eq!(*old.lock().unwrap(), 42);

        #[cfg(miri)]
        unsafe {
            drop(Box::from_raw(leaked.unwrap().as_ptr())); // no leaks under miri
        }
    }

    #[test]
    fn is_send_and_sync_for_send_values() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<LeakableMutex<usize>>();
    }
}

/// A `static` owning a `Box<T>` that a forked child abandons rather than
/// frees.
///
/// The sibling of [`LeakableMutex`], for state that is not behind a mutex
/// because something else already serialises it. The memory profiler's heap
/// tracker is the motivating case: it is only ever touched from an allocator
/// hook with the GIL held, so a mutex would add an atomic round trip to the
/// hottest path in the process and, worse, a self-deadlock hazard. If anything
/// on the sampling path ever allocated through PyMem, re-entering while a
/// `MutexGuard` was alive would hang the interpreter with the GIL held.
/// Publishing through an [`AtomicPtr`] instead makes the same mistake degrade
/// to a skipped sample.
///
/// [`leak`](LeakablePtr::leak) exists for the same reason as
/// `LeakableMutex::leak_and_reset`: after `fork()` the child must not drop
/// state inherited from the parent. Here that matters twice over, because
/// dropping frees through libc and **macOS libmalloc is not fork-safe** in a
/// `fork()`-without-`exec()` child.
///
/// # How to use it
///
/// ```ignore
/// static TRACKER: LeakablePtr<Tracker> = LeakablePtr::new();
///
/// // Publish (once, at start-up):
/// TRACKER.publish(Box::new(Tracker::new()));
///
/// // Use, from anywhere:
/// let ptr = TRACKER.load(Ordering::Acquire);
/// if !ptr.is_null() { /* ... */ }
///
/// // Tear down, dropping the value:
/// let old = TRACKER.take();
///
/// // In the post-fork child hook: abandon it instead.
/// TRACKER.leak();
/// ```
///
/// The accessors take `&'static self` for the same reason as
/// [`LeakableMutex`]: this type is only meant to live in a `static`.
// The only user is the memory profiler's heap tracker, which is behind the
// `memory` feature. Keep the type compiled in every configuration so it is
// type-checked and its tests run either way.
#[cfg_attr(not(feature = "memory"), allow(dead_code))]
pub struct LeakablePtr<T> {
    state: AtomicPtr<T>,
    // AtomicPtr does not inherit T's auto-traits. Model ownership of the
    // pointee so LeakablePtr has the same ones as Box<T>.
    _marker: PhantomData<Box<T>>,
}

#[cfg_attr(not(feature = "memory"), allow(dead_code))]
impl<T> LeakablePtr<T> {
    /// Creates an empty `LeakablePtr`.
    ///
    /// `const`, so it can be used in a `static`.
    pub const fn new() -> Self {
        Self {
            state: AtomicPtr::new(std::ptr::null_mut()),
            _marker: PhantomData,
        }
    }

    /// Publishes `value`, returning the raw pointer now stored.
    ///
    /// A `Release` store, so everything written into `value` before publishing
    /// is visible to any thread that subsequently loads the pointer with
    /// `Acquire`. Any previously published pointer is returned to the caller
    /// to dispose of.
    #[must_use = "the previously published pointer must be dropped or leaked"]
    pub fn publish(&'static self, value: Box<T>) -> *mut T {
        self.state.swap(Box::into_raw(value), Ordering::AcqRel)
    }

    /// Loads the published pointer, or null if there is none.
    ///
    /// Callers must use `Acquire` to pair with [`publish`](LeakablePtr::publish).
    pub fn load(&'static self, ordering: Ordering) -> *mut T {
        self.state.load(ordering)
    }

    /// Unpublishes and returns the current pointer, so the caller can drop it.
    ///
    /// `AcqRel`, so writes made through the pointer by other threads are
    /// visible to whoever drops it.
    #[must_use = "the returned pointer must be dropped or leaked"]
    pub fn take(&'static self) -> *mut T {
        self.state.swap(std::ptr::null_mut(), Ordering::AcqRel)
    }

    /// Abandons the published value without dropping it.
    ///
    /// Intended only for a post-fork child hook. The allocation is
    /// deliberately leaked: freeing memory inherited from the parent is the
    /// exact hazard this exists to avoid.
    #[cfg(not(miri))]
    pub fn leak(&'static self) {
        let _ = self.state.swap(std::ptr::null_mut(), Ordering::AcqRel);
    }

    /// Miri-only variant of [`leak`](LeakablePtr::leak) (see that item for the
    /// full contract). It hands back the abandoned allocation so tests can
    /// reclaim it and keep Miri's leak checker happy, or `None` if nothing was
    /// published.
    #[cfg(miri)]
    #[must_use = "reclaim the returned allocation or Miri reports a leak"]
    pub fn leak(&'static self) -> Option<std::ptr::NonNull<T>> {
        std::ptr::NonNull::new(self.state.swap(std::ptr::null_mut(), Ordering::AcqRel))
    }
}

// No `Drop` impl, for the same reason as LeakableMutex: `publish` takes
// `&'static self`, so an instance that could be dropped can never have
// published anything.

#[cfg(test)]
mod leakable_ptr_tests {
    use super::LeakablePtr;
    use std::sync::atomic::Ordering;

    /// Each test needs its own `static`, because the accessors take
    /// `&'static self`. A value still reachable through a static at exit is
    /// not a leak for Miri, whose checker is reachability-based; only what
    /// `leak` abandons needs reclaiming.
    #[test]
    fn starts_empty() {
        static PTR: LeakablePtr<u32> = LeakablePtr::new();
        assert!(PTR.load(Ordering::Acquire).is_null());
    }

    #[test]
    fn publish_then_load_then_take() {
        static PTR: LeakablePtr<u32> = LeakablePtr::new();
        let previous = PTR.publish(Box::new(7));
        assert!(previous.is_null(), "nothing was published before");

        let loaded = PTR.load(Ordering::Acquire);
        assert!(!loaded.is_null());
        // SAFETY: we published this pointer and have not taken it back.
        assert_eq!(unsafe { *loaded }, 7);

        let taken = PTR.take();
        assert_eq!(taken, loaded);
        assert!(PTR.load(Ordering::Acquire).is_null());
        // SAFETY: `take` transferred ownership back to us.
        drop(unsafe { Box::from_raw(taken) });
    }

    #[test]
    fn publishing_again_returns_the_old_pointer() {
        static PTR: LeakablePtr<u32> = LeakablePtr::new();
        let first = PTR.publish(Box::new(1));
        assert!(first.is_null());
        let replaced = PTR.publish(Box::new(2));
        assert!(!replaced.is_null());
        // SAFETY: `publish` handed ownership of the old value back to us.
        assert_eq!(unsafe { *replaced }, 1);
        drop(unsafe { Box::from_raw(replaced) });

        let current = PTR.take();
        // SAFETY: `take` transferred ownership back to us.
        assert_eq!(unsafe { *current }, 2);
        drop(unsafe { Box::from_raw(current) });
    }

    #[test]
    fn leak_abandons_without_dropping() {
        static PTR: LeakablePtr<u32> = LeakablePtr::new();
        let previous = PTR.publish(Box::new(99));
        assert!(previous.is_null());

        #[cfg(miri)]
        let leaked = PTR.leak();
        #[cfg(not(miri))]
        PTR.leak();

        assert!(
            PTR.load(Ordering::Acquire).is_null(),
            "leak did not unpublish"
        );

        // Under Miri, reclaim what was abandoned so the leak checker stays
        // quiet; in production leaking is the whole point.
        #[cfg(miri)]
        // SAFETY: `leak` handed back the allocation it abandoned.
        drop(unsafe { Box::from_raw(leaked.expect("something was published").as_ptr()) });
    }

    #[test]
    fn taking_from_an_empty_slot_is_null() {
        static PTR: LeakablePtr<u32> = LeakablePtr::new();
        assert!(PTR.take().is_null());
    }
}
