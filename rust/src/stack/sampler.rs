use super::Options;

unsafe extern "C" {
    fn pyroscope_stack_configure(interval_s: f64, max_nframes: u32, max_threads: u32);
    fn pyroscope_stack_is_safe_copy_failed() -> bool;
    fn pyroscope_stack_start() -> bool;
    fn pyroscope_stack_stop();
    fn pyroscope_stack_take_sampling_thread_error() -> bool;
}

pub(super) fn configure(interval_s: f64, options: &Options) {
    if cfg!(miri) {
        return;
    }
    unsafe { pyroscope_stack_configure(interval_s, options.max_nframe, options.max_threads) }
}

pub(super) fn is_safe_copy_failed() -> bool {
    unsafe { pyroscope_stack_is_safe_copy_failed() }
}

pub(super) fn sampler_start() -> bool {
    unsafe { pyroscope_stack_start() }
}

pub(super) fn sampler_stop() {
    unsafe { pyroscope_stack_stop() }
}

pub(super) fn sampling_thread_failed() -> bool {
    unsafe { pyroscope_stack_take_sampling_thread_error() }
}
