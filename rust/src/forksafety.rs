use std::sync::Mutex;
use std::sync::atomic::{AtomicPtr, Ordering};

pub struct LeakableMutex<T> {
    state: AtomicPtr<Mutex<T>>,
}
impl<T: Default> LeakableMutex<T> {
    pub const fn new() -> Self {
        Self {
            state: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

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

    pub fn leak_and_reset(&self) {
        self.state.store(Self::new_static(), Ordering::SeqCst)
    }

    fn new_static() -> *mut Mutex<T> {
        Box::into_raw(Box::new(Mutex::new(T::default())))
    }
}
