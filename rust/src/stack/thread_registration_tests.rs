use std::sync::mpsc;

unsafe extern "C" {
    fn pyroscope_stack_register_thread(id: u64, native_id: u64);
    fn pyroscope_stack_unregister_thread(id: u64);
    fn pyroscope_stack_thread_count() -> usize;
}

/// The ids must be live pthread_t values: `ThreadInfo::create` calls
/// `pthread_getcpuclockid` / `pthread_mach_thread_np` on them, which
/// dereferences the pthread descriptor.
#[test]
fn threads_register_and_unregister_over_the_ffi() {
    let before = unsafe { pyroscope_stack_thread_count() };

    let (registered_tx, registered_rx) = mpsc::channel();

    let mut handles = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..2 {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let registered_tx = registered_tx.clone();
        handles.push(std::thread::spawn(move || {
            let id = unsafe { libc::pthread_self() } as u64;
            unsafe { pyroscope_stack_register_thread(id, id) };
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
