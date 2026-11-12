pub use pyroscope_pprof::*;

use crate::backend::Report;

/// Adapt SDK reports to the transport-independent pprof builder.
pub(crate) fn encode_reports(
    reports: Vec<Report>,
    sample_rate: u32,
    time_range: TimeRange,
) -> pyroscope_pprof::proto::Profile {
    let mut strings = StringTable::new();
    let mut builder = PProfBuilder::<PySpyProfile>::new();
    builder.set_time_range(&time_range);
    builder.set_profile_type(&mut strings, sample_rate);
    for report in reports {
        for (stack, value) in report.data {
            builder.add_stacktrace(
                &mut strings,
                stack
                    .frames
                    .iter()
                    .map(|f| (f.name.as_str(), f.filename.as_str(), i64::from(f.line))),
                stack
                    .metadata
                    .tags
                    .iter()
                    .map(|tag| (tag.key.as_str(), tag.value.as_str())),
                value,
            );
        }
    }
    builder.into_profile(strings)
}
