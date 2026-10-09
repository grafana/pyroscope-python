#[test]
fn a_sampler_that_never_started_stashed_no_error() {
    assert!(!super::sampling_thread_failed());
}
