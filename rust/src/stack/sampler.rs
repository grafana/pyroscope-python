use super::Options;

unsafe extern "C" {
    fn pyroscope_stack_bump_upload_seq();
    fn pyroscope_stack_configure(
        interval_s: f64,
        fast_copy: bool,
        fast_copy_warmup_s: f64,
        max_nframes: u32,
        max_threads: u32,
        max_tasks: u32,
        adaptive_sampling: bool,
        target_overhead: f64,
        max_sampling_period_us: u64,
        baseline_core_pct: f64,
        p_stable_window_s: u32,
        p_stable_percentile: f64,
    );
    fn pyroscope_stack_fast_copy_initialized() -> bool;
    fn pyroscope_stack_interval_us() -> u64;
    fn pyroscope_stack_is_safe_copy_failed() -> bool;
    fn pyroscope_stack_start() -> bool;
    fn pyroscope_stack_stop();
    fn pyroscope_stack_take_sampling_thread_error() -> bool;
}

pub(super) fn bump_upload_seq() {
    if !cfg!(miri) {
        unsafe { pyroscope_stack_bump_upload_seq() }
    }
}

pub(super) fn configure(interval_s: f64, options: &Options) {
    if cfg!(miri) {
        return;
    }
    unsafe {
        pyroscope_stack_configure(
            interval_s,
            options.fast_copy,
            options.fast_copy_warmup_s,
            options.max_nframe,
            options.max_threads,
            options.max_tasks,
            options.adaptive_sampling,
            options.adaptive_target_overhead,
            options.adaptive_max_interval_us,
            options.adaptive_baseline,
            options.adaptive_p_stable_window_s,
            options.adaptive_p_stable_percentile,
        )
    }
}

pub(super) fn fast_copy_initialized() -> bool {
    unsafe { pyroscope_stack_fast_copy_initialized() }
}

pub(super) fn interval_us() -> u64 {
    if cfg!(miri) {
        return 0;
    }
    unsafe { pyroscope_stack_interval_us() }
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
