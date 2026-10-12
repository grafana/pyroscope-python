use super::*;
use crate::encode::r#gen::google::Profile;
use crate::encode::interner;
use crate::encode::pprof::PprofBuilderType;
use crate::encode::pprof::ffi::FFIInternedString;
use std::time::{Duration, UNIX_EPOCH};

fn intern(s: &str) -> FFIInternedString {
    (&interner::string_table().lock().unwrap().add(s)).into()
}

/// The memory slots are non-zero so that one leaking into a cpu or wall
/// value cannot pass for a correct number.
fn values(cpu_time: i64, wall_time: i64) -> FFISampleValues {
    FFISampleValues {
        cpu_time,
        wall_time,
        alloc_space: 100_000,
        alloc_count: 200_000,
        heap_space: 300_000,
        heap_count: 400_000,
    }
}

fn push(builder_type: PprofBuilderType, frames: &[FFIFrame], values: &FFISampleValues) {
    crate::ffi::pyroscope_push_sample(builder_type, frames.as_ptr(), frames.len(), values);
}

fn resolve(profile: &Profile, index: i64) -> &str {
    profile.string_table[index as usize].as_str()
}

#[test]
fn cpu_options_are_fixed_by_the_first_call() {
    let lock = OnceLock::new();

    assert!(set_options_in(
        &lock,
        Options {
            max_nframe: 7,
            ..Options::default()
        }
    ));
    assert!(!set_options_in(
        &lock,
        Options {
            max_nframe: 9,
            ..Options::default()
        }
    ));
    assert_eq!(lock.get().expect("the first call seals").max_nframe, 7);
}

#[test]
fn invalid_adaptive_options_are_refused_without_sealing() {
    let invalid = [
        Options {
            adaptive_target_overhead: 0.0,
            ..Options::default()
        },
        Options {
            adaptive_target_overhead: 1.5,
            ..Options::default()
        },
        Options {
            adaptive_target_overhead: f64::NAN,
            ..Options::default()
        },
        Options {
            adaptive_max_interval_us: 0,
            ..Options::default()
        },
        Options {
            adaptive_baseline: -1.0,
            ..Options::default()
        },
        Options {
            adaptive_baseline: f64::INFINITY,
            ..Options::default()
        },
        Options {
            adaptive_p_stable_window_s: 0,
            ..Options::default()
        },
        Options {
            adaptive_p_stable_percentile: 101.0,
            ..Options::default()
        },
        Options {
            adaptive_p_stable_percentile: f64::NAN,
            ..Options::default()
        },
    ];
    let lock = OnceLock::new();
    for options in invalid {
        let debug = format!("{options:?}");
        assert!(!set_options_in(&lock, options), "{debug}");
    }
    assert!(lock.get().is_none());
    assert!(set_options_in(&lock, Options::default()));
}

/// Deliberately a single test: it drains the process-wide accumulator and
/// moves the sampler's interval, so a second test touching either in
/// parallel would race.
#[test]
fn cpu_wall_samples_pushed_over_the_ffi_become_one_profile() {
    configure(
        1.0 / 50.0,
        &Options {
            adaptive_sampling: false,
            adaptive_target_overhead: 0.05,
            adaptive_max_interval_us: 500_000,
            adaptive_baseline: 1.0,
            adaptive_p_stable_window_s: 5,
            adaptive_p_stable_percentile: 50.0,
            ..Options::default()
        },
        false,
    );

    let frames = [FFIFrame {
        function_name: intern("stack::tests::some_function"),
        file_name: intern("stack::tests/some_file.py"),
        line: 42,
    }];
    let time_range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();

    push(PprofBuilderType::CpuWall, &frames, &values(3, 5));
    push(PprofBuilderType::CpuWall, &frames, &values(7, 11));

    let bytes = dump_pprof(100, false, &time_range).expect("a profile with one sample");
    let profile = Profile::decode(bytes.as_slice()).expect("a decodable pprof");

    let sample_types: Vec<(&str, &str)> = profile
        .sample_type
        .iter()
        .map(|vt| (resolve(&profile, vt.r#type), resolve(&profile, vt.unit)))
        .collect();
    assert_eq!(
        sample_types,
        vec![("cpu", "nanoseconds"), ("wall", "nanoseconds")]
    );
    let period_type = profile.period_type.as_ref().expect("a period type");
    assert_eq!(
        (
            resolve(&profile, period_type.r#type),
            resolve(&profile, period_type.unit)
        ),
        ("cpu", "nanoseconds")
    );
    let expected_period = if cfg!(miri) { 10_000_000 } else { 20_000_000 };
    assert_eq!(
        profile.period, expected_period,
        "the sampler's interval, not 1 / sample_rate"
    );
    assert_eq!(profile.duration_nanos, 10_000_000_000);

    assert_eq!(profile.sample.len(), 1, "one row per distinct stack");
    assert_eq!(profile.sample[0].value, vec![10, 16]);
    assert_eq!(profile.function.len(), 1);
    assert_eq!(
        resolve(&profile, profile.function[0].name),
        "stack::tests::some_function"
    );
    assert_eq!(
        resolve(&profile, profile.function[0].filename),
        "stack::tests/some_file.py"
    );
    assert_eq!(profile.location.len(), 1);
    assert_eq!(profile.location[0].line[0].line, 42);

    assert!(
        dump_pprof(100, false, &time_range).is_none(),
        "a drained accumulator must not produce a second profile"
    );

    push(PprofBuilderType::CpuWall, &frames, &values(1, 2));
    clear_samples();
    assert!(dump_pprof(100, false, &time_range).is_none());

    push(PprofBuilderType::OnCpu, &frames, &values(3, 5));
    let bytes = dump_pprof(100, true, &time_range).expect("an oncpu profile with one sample");
    let profile = Profile::decode(bytes.as_slice()).expect("a decodable pprof");
    let sample_types: Vec<(&str, &str)> = profile
        .sample_type
        .iter()
        .map(|vt| (resolve(&profile, vt.r#type), resolve(&profile, vt.unit)))
        .collect();
    assert_eq!(sample_types, vec![("cpu", "nanoseconds")]);
    assert_eq!(profile.sample.len(), 1);
    assert_eq!(profile.sample[0].value, vec![3]);
}
