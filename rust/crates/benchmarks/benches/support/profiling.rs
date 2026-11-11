//! Criterion's profile mode gives each individual case a fresh pyroscope-rs CPU sampler.

use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use criterion::profiler::Profiler;
use prost::Message;
use pyroscope::backend::{
    BackendConfig, BackendImpl, BackendReady, PprofConfig, ReportData, pprof_backend,
};

const SAMPLE_RATE: u32 = 100;

struct Active {
    backend: BackendImpl<BackendReady>,
    started: Instant,
    start_time_nanos: u64,
}

#[derive(Default)]
pub struct Pprof {
    active: Option<Active>,
}

impl Profiler for Pprof {
    fn start_profiling(&mut self, _benchmark_id: &str, _benchmark_dir: &Path) {
        assert!(self.active.is_none(), "overlapping benchmark profiles");
        let backend = pprof_backend(
            PprofConfig {
                sample_rate: SAMPLE_RATE,
            },
            BackendConfig {
                report_thread_id: true,
                ..BackendConfig::default()
            },
        )
        .initialize()
        .expect("failed to start pyroscope-rs CPU sampler");
        self.active = Some(Active {
            backend,
            started: Instant::now(),
            start_time_nanos: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .try_into()
                .unwrap(),
        });
    }

    fn stop_profiling(&mut self, benchmark_id: &str, benchmark_dir: &Path) {
        let mut active = self.active.take().expect("missing benchmark profile");
        let duration_nanos = active.started.elapsed().as_nanos().try_into().unwrap();
        let batch = active.backend.report();
        // Stop and clear the sampler before encoding, writing files, or starting another case.
        active
            .backend
            .shutdown()
            .expect("failed to stop CPU sampler");
        let ReportData::Reports(reports) = batch.expect("failed to collect CPU samples").data
        else {
            panic!("CPU sampler returned an unexpected report format");
        };
        let mut profile = pyroscope::encode::pprof::encode(
            &reports,
            SAMPLE_RATE,
            active.start_time_nanos,
            duration_nanos,
        );
        let total_cpu_ns: i64 = profile.sample.iter().flat_map(|s| &s.value).sum();
        assert!(
            total_cpu_ns > 0,
            "{benchmark_id}: no CPU samples; use a longer --profile-time; full profile: {profile:#?}"
        );
        profile.comment.push(profile.string_table.len() as i64);
        profile.string_table.push(benchmark_id.to_owned());
        std::fs::create_dir_all(benchmark_dir).expect("failed to create profile directory");
        let path = benchmark_dir.join("profile.pprof");
        std::fs::write(&path, profile.encode_to_vec()).expect("failed to write pprof");
        eprintln!(
            "Wrote {} ({} samples at {SAMPLE_RATE} Hz)",
            path.display(),
            total_cpu_ns / (1_000_000_000 / i64::from(SAMPLE_RATE)),
        );
    }
}
