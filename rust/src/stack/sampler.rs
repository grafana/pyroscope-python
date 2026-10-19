use super::Options;

unsafe extern "C" {
    fn pyroscope_stack_configure(
        interval_s: f64,
        max_nframes: u32,
        max_threads: u32,
        adaptive_sampling: bool,
        target_overhead: f64,
        max_sampling_period_us: u64,
        baseline_core_pct: f64,
        p_stable_window_s: u32,
        p_stable_percentile: f64,
    ) -> bool;
    fn pyroscope_stack_interval_us() -> u64;
    fn pyroscope_stack_start() -> bool;
    fn pyroscope_stack_stop();
    fn pyroscope_stack_take_sampling_thread_error() -> bool;
}

pub(super) fn configure(interval_s: f64, options: &Options) -> bool {
    if cfg!(miri) {
        return true;
    }
    unsafe {
        pyroscope_stack_configure(
            interval_s,
            options.max_nframe,
            options.max_threads,
            options.adaptive_sampling,
            options.adaptive_target_overhead,
            options.adaptive_max_interval_us,
            options.adaptive_baseline,
            options.adaptive_p_stable_window_s,
            options.adaptive_p_stable_percentile,
        )
    }
}

pub(super) fn interval_us() -> u64 {
    if cfg!(miri) {
        return 0;
    }
    unsafe { pyroscope_stack_interval_us() }
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
