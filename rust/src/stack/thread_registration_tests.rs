use std::sync::mpsc;

unsafe extern "C" {
    fn pyroscope_stack_register_thread(id: u64, native_id: u64);
    fn pyroscope_stack_unregister_thread(id: u64);
    fn pyroscope_stack_set_uvloop_mode(thread_id: u64, value: bool) -> bool;
}

#[test]
fn uvloop_mode_reaches_registered_threads_only() {
    let (registered_tx, registered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let id = unsafe { libc::pthread_self() } as u64;
        unsafe { pyroscope_stack_register_thread(id, id) };
        registered_tx.send(id).unwrap();
        release_rx.recv().unwrap();
        unsafe { pyroscope_stack_unregister_thread(id) };
    });

    let id = registered_rx.recv().unwrap();
    let unregistered = id ^ 0xffff_ffff;

    assert!(unsafe { pyroscope_stack_set_uvloop_mode(id, true) });
    assert!(unsafe { pyroscope_stack_set_uvloop_mode(id, false) });
    assert!(!unsafe { pyroscope_stack_set_uvloop_mode(unregistered, true) });

    release_tx.send(()).unwrap();
    handle.join().unwrap();
}
