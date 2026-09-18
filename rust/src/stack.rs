use crate::encode::pprof::ffi::{FFIFrame, FFISampleValues};
use crate::encode::pprof::{CpuWallProfile, PProfBuilder};
use crate::utils::TimeRange;
use lazy_static::lazy_static;
use prost::Message;
use std::ops::{Deref, DerefMut};
use std::sync::Mutex;

lazy_static! {
    static ref PROFILE_BUILDER: Mutex<PProfBuilder<CpuWallProfile>> =
        Mutex::new(PProfBuilder::new());
}

#[cfg(feature = "memory")]
unsafe extern "C" {
    fn pyroscope_stack_bump_upload_seq();
}

#[cfg(all(test, feature = "memory"))]
unsafe extern "C" {
    fn pyroscope_stack_upload_seq() -> u64;
}

#[cfg(feature = "memory")]
fn bump_upload_seq() {
    unsafe { pyroscope_stack_bump_upload_seq() }
}

#[cfg(not(feature = "memory"))]
fn bump_upload_seq() {}

pub fn push_sample(frames: &[FFIFrame], values: &FFISampleValues) {
    if let Ok(mut pb) = PROFILE_BUILDER.lock() {
        pb.add_ffi_sample(frames, values);
    }
}

/// Discard the samples buffered for the next cpu/wall profile.
///
/// See `crate::memory::implementation::clear_samples` for the reasoning,
/// including why the shared string table is deliberately left alone.
pub fn clear_samples() {
    let mut pb = PROFILE_BUILDER.lock().unwrap_or_else(|e| e.into_inner());
    pb.reset();
}

/// Take the accumulated cpu/wall samples as an encoded pprof, if any.
///
/// Unlike `crate::memory::dump_pprof` this needs neither the GIL nor a
/// profiler-side flush, but it keeps the same lock order: the interner before
/// the profile builder, never the reverse.
pub fn dump_pprof(sample_rate: u32, time_range: &TimeRange) -> Option<Vec<u8>> {
    let st = crate::encode::interner::string_table().lock();
    let pb = PROFILE_BUILDER.lock();
    let profile = match (st, pb) {
        (Ok(mut st), Ok(mut pb)) => {
            pb.set_profile_type(st.deref_mut(), sample_rate);
            pb.take_profile_and_reset(st.deref(), time_range)
        }
        _ => None,
    }?;
    bump_upload_seq();
    Some(profile.encode_to_vec())
}

#[cfg(test)]
mod tests {
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

    fn push(frames: &[FFIFrame], values: &FFISampleValues) {
        crate::ffi::pyroscope_push_sample(
            PprofBuilderType::CpuWall,
            frames.as_ptr(),
            frames.len(),
            values,
        );
    }

    fn resolve(profile: &Profile, index: i64) -> &str {
        profile.string_table[index as usize].as_str()
    }

    #[cfg(feature = "memory")]
    fn upload_seq() -> Option<u64> {
        Some(unsafe { pyroscope_stack_upload_seq() })
    }

    #[cfg(not(feature = "memory"))]
    fn upload_seq() -> Option<u64> {
        None
    }

    /// Deliberately a single test: it drains the process-wide accumulator, so a
    /// second test touching it in parallel would race.
    #[test]
    fn cpu_wall_samples_pushed_over_the_ffi_become_one_profile() {
        let frames = [FFIFrame {
            function_name: intern("stack::tests::some_function"),
            file_name: intern("stack::tests/some_file.py"),
            line: 42,
        }];
        let time_range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();

        push(&frames, &values(3, 5));
        push(&frames, &values(7, 11));

        let seq_before = upload_seq();
        let bytes = dump_pprof(100, &time_range).expect("a profile with one sample");
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
        assert_eq!(profile.period, 10_000_000);
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

        let seq_after_dump = upload_seq();
        assert_eq!(
            seq_after_dump,
            seq_before.map(|s| s + 1),
            "one upload, one bump"
        );

        assert!(
            dump_pprof(100, &time_range).is_none(),
            "a drained accumulator must not produce a second profile"
        );
        assert_eq!(
            upload_seq(),
            seq_after_dump,
            "an empty window is not an upload"
        );

        push(&frames, &values(1, 2));
        clear_samples();
        assert!(dump_pprof(100, &time_range).is_none());
    }
}
