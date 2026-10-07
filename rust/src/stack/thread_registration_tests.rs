use pyo3::ffi::PyObject;
use std::ffi::{CString, c_char};
use std::sync::{Mutex, mpsc};

unsafe extern "C" {
    fn pyroscope_stack_register_thread(id: u64, native_id: u64, name: *const c_char);
    fn pyroscope_stack_unregister_thread(id: u64);
    fn pyroscope_stack_thread_count() -> usize;
    fn pyroscope_stack_track_asyncio_loop(thread_id: u64, event_loop: *mut PyObject);
    fn pyroscope_stack_thread_asyncio_loop(thread_id: u64) -> usize;
    fn pyroscope_stack_set_uvloop_mode(thread_id: u64, value: bool) -> bool;
}

/// Both tests mutate echion's process-wide thread map, and the count
/// assertions below are deltas.
static THREAD_MAP: Mutex<()> = Mutex::new(());

/// The ids must be live pthread_t values: `ThreadInfo::create` calls
/// `pthread_getcpuclockid` / `pthread_mach_thread_np` on them, which
/// dereferences the pthread descriptor.
#[test]
fn threads_register_and_unregister_over_the_ffi() {
    let _guard = THREAD_MAP.lock().unwrap_or_else(|e| e.into_inner());
    let before = unsafe { pyroscope_stack_thread_count() };

    let (registered_tx, registered_rx) = mpsc::channel();
    let name = CString::new("stack::registration_test").unwrap();

    let mut handles = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..2 {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let registered_tx = registered_tx.clone();
        let name = name.clone();
        handles.push(std::thread::spawn(move || {
            let id = unsafe { libc::pthread_self() } as u64;
            unsafe { pyroscope_stack_register_thread(id, id, name.as_ptr()) };
            registered_tx.send(id).unwrap();
            release_rx.recv().unwrap();
            unsafe { pyroscope_stack_unregister_thread(id) };
        }));
        releases.push(release_tx);
    }

    let ids: Vec<u64> = (0..2).map(|_| registered_rx.recv().unwrap()).collect();
    assert_ne!(ids[0], ids[1], "two distinct threads");
    assert_eq!(
        unsafe { pyroscope_stack_thread_count() },
        before + 2,
        "both threads registered"
    );

    for release in releases {
        release.send(()).unwrap();
    }
    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(
        unsafe { pyroscope_stack_thread_count() },
        before,
        "both threads unregistered"
    );
}

#[test]
fn a_sampler_that_never_started_stashed_no_error() {
    assert!(!super::sampling_thread_failed());
}

/// `track_asyncio_loop` is a find-then-mutate on echion's thread map, so an
/// unregistered thread takes the loop silently and never unwinds tasks.
/// This is what pins `asyncio::install` after `threads::install`.
#[test]
fn asyncio_loops_reach_registered_threads_only() {
    let _guard = THREAD_MAP.lock().unwrap_or_else(|e| e.into_inner());

    let (registered_tx, registered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let name = CString::new("stack::asyncio_test").unwrap();
    let handle = std::thread::spawn(move || {
        let id = unsafe { libc::pthread_self() } as u64;
        unsafe { pyroscope_stack_register_thread(id, id, name.as_ptr()) };
        registered_tx.send(id).unwrap();
        release_rx.recv().unwrap();
        unsafe { pyroscope_stack_unregister_thread(id) };
    });

    let id = registered_rx.recv().unwrap();
    let unregistered = id ^ 0xffff_ffff;
    let event_loop = 0x1234_usize as *mut PyObject;

    unsafe { pyroscope_stack_track_asyncio_loop(id, event_loop) };
    unsafe { pyroscope_stack_track_asyncio_loop(unregistered, event_loop) };

    assert_eq!(
        unsafe { pyroscope_stack_thread_asyncio_loop(id) },
        0x1234,
        "the loop reached the registered thread"
    );
    assert_eq!(
        unsafe { pyroscope_stack_thread_asyncio_loop(unregistered) },
        0,
        "the loop was dropped for an unregistered thread"
    );

    release_tx.send(()).unwrap();
    handle.join().unwrap();
}

#[test]
fn uvloop_mode_reaches_registered_threads_only() {
    let _guard = THREAD_MAP.lock().unwrap_or_else(|e| e.into_inner());

    let (registered_tx, registered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let name = CString::new("stack::uvloop_test").unwrap();
    let handle = std::thread::spawn(move || {
        let id = unsafe { libc::pthread_self() } as u64;
        unsafe { pyroscope_stack_register_thread(id, id, name.as_ptr()) };
        registered_tx.send(id).unwrap();
        release_rx.recv().unwrap();
        unsafe { pyroscope_stack_unregister_thread(id) };
    });

    let id = registered_rx.recv().unwrap();
    let unregistered = id ^ 0xffff_ffff;

    assert!(
        unsafe { pyroscope_stack_set_uvloop_mode(id, true) },
        "the registered thread took uvloop mode"
    );
    assert!(
        unsafe { pyroscope_stack_set_uvloop_mode(id, false) },
        "the registered thread took the mode back off"
    );
    assert!(
        !unsafe { pyroscope_stack_set_uvloop_mode(unregistered, true) },
        "uvloop mode was dropped for an unregistered thread"
    );

    release_tx.send(()).unwrap();
    handle.join().unwrap();
}
