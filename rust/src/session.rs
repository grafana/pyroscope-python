use std::{
    io::Write,
    sync::mpsc::{Receiver, SyncSender, sync_channel},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::encode::r#gen::push::{PushRequest, RawProfileSeries, RawSample};
use crate::encode::r#gen::types::LabelPair;
use crate::utils::TimeRange;
use crate::{
    Result,
    backend::{ReportBatch, ReportData, Tag},
    encode::pprof::encode_reports,
    pyroscope::PyroscopeConfig,
};
use libflate::gzip::Encoder;
use prost::Message;
use reqwest::Url;
use uuid::Uuid;

const LOG_TAG: &str = "Pyroscope::Session";
const LABEL_SCOPE_NAME: &str = "otel.scope.name";
const LABEL_SCOPE_VERSION: &str = "otel.scope.version";
const LABEL_PROCESS_RUNTIME_NAME: &str = "process.runtime.name";
const LABEL_PROCESS_RUNTIME_VERSION: &str = "process.runtime.version";
const LABEL_SERVICE_NAME: &str = "service_name";
const LABEL_PROFILE_NAME: &str = "__name__";
const SCOPE_NAME: &str = "com.grafana.pyroscope/python";

/// Session Signal
///
/// This enum is used to send data to the session thread. It can also kill the session thread.
pub enum SessionSignal {
    /// Send session data to the session thread.
    Session(Box<Session>),
    /// Kill the session thread.
    Kill,
}

/// Manage sessions and send data to the server.
#[derive(Debug)]
pub struct SessionManager {
    /// The SessionManager thread.
    pub handle: Option<JoinHandle<Result<()>>>,
    /// Channel to send data to the SessionManager thread.
    pub tx: SyncSender<SessionSignal>,
}

impl SessionManager {
    /// Create a new SessionManager
    pub fn new(client: reqwest::blocking::Client) -> Self {
        log::info!(target: LOG_TAG, "Creating SessionManager");

        // Create a channel for sending and receiving sessions
        let (tx, rx): (SyncSender<SessionSignal>, Receiver<SessionSignal>) = sync_channel(10);

        // Create a thread for the SessionManager
        let handle = Some(thread::spawn(move || {
            log::trace!(target: LOG_TAG, "Started");
            while let Ok(signal) = rx.recv() {
                match signal {
                    SessionSignal::Session(session) => {
                        // Send the session
                        // Matching is done here (instead of ?) to avoid breaking
                        // the SessionManager thread if the server is not available.
                        match (*session).push(&client) {
                            Ok(_) => log::trace!("SessionManager - Session sent"),
                            Err(e) => log::error!("SessionManager - Failed to send session: {e}"),
                        }
                    }
                    SessionSignal::Kill => {
                        // Kill the session manager
                        log::trace!(target: LOG_TAG, "Kill signal received");
                        return Ok(());
                    }
                }
            }
            Ok(())
        }));

        SessionManager { handle, tx }
    }

    /// Push a new session into the SessionManager
    pub fn push(&self, session: SessionSignal) -> Result<()> {
        // Push the session into the SessionManager
        self.tx.send(session)?;

        log::trace!(target: LOG_TAG, "SessionSignal pushed");

        Ok(())
    }
}

pub struct Session {
    pub config: PyroscopeConfig,
    pub batch: Vec<ReportBatch>,
    time_range: TimeRange,
}

impl Session {
    pub fn new(time_range: TimeRange, config: PyroscopeConfig, batch: Vec<ReportBatch>) -> Self {
        Self {
            config,
            batch,
            time_range,
        }
    }

    fn push(self, client: &reqwest::blocking::Client) -> Result<()> {
        log::info!(target: LOG_TAG, "Sending Session: {:?} ", self.time_range);

        let req = build_push_request(&self.config, &self.time_range, self.batch);

        let req = Self::gzip(&req.encode_to_vec())?;

        let mut url = Url::parse(&self.config.url)?;
        url.path_segments_mut()
            .unwrap()
            .push("push.v1.PusherService")
            .push("Push");

        let mut req_builder = client
            .post(url.as_str())
            .header(
                "User-Agent",
                format!(
                    "pyroscope-rs/{}/{} reqwest",
                    self.config.spy_name, self.config.spy_version
                ),
            )
            .header("Content-Type", "application/proto")
            .header("Content-Encoding", "gzip");

        if let Some(basic_auth) = &self.config.basic_auth {
            req_builder = req_builder.basic_auth(
                basic_auth.username.clone(),
                Some(basic_auth.password.clone()),
            );
        }
        if let Some(tenant_id) = &self.config.tenant_id {
            req_builder = req_builder.header("X-Scope-OrgID", tenant_id);
        }
        for (k, v) in &self.config.http_headers {
            req_builder = req_builder.header(k, v);
        }

        let mut response = req_builder
            .body(req)
            .timeout(Duration::from_secs(10))
            .send()?;

        let status = response.status();

        if status.is_success() {
            let mut sink = std::io::sink();
            _ = response.copy_to(&mut sink);
        } else {
            let resp = response.text();
            let resp = match &resp {
                Ok(t) => t,
                Err(_) => "",
            };
            log::error!(target: LOG_TAG, "Sending Session failed {} {}", status.as_u16(), resp);
        }
        Ok(())
    }

    fn gzip(report: &[u8]) -> Result<Vec<u8>> {
        let mut encoder = Encoder::new(Vec::new())?;
        encoder.write_all(report)?;
        let compressed_data = encoder.finish().into_result()?;
        Ok(compressed_data)
    }
}

fn build_push_request(
    config: &PyroscopeConfig,
    time_range: &TimeRange,
    batch: Vec<ReportBatch>,
) -> PushRequest {
    let mut req = PushRequest {
        series: Vec::with_capacity(batch.len()),
    };
    for batch in batch {
        let ReportBatch {
            profile_type,
            data,
            series_tags,
        } = batch;

        let raw_profile = match data {
            ReportData::RawPprof(pprof_bytes) => pprof_bytes,
            ReportData::Reports(reports) => {
                encode_reports(reports, config.sample_rate, time_range.clone()).encode_to_vec()
            }
        };

        let labels = labels_for_profile(config, profile_type, &series_tags);
        let series = RawProfileSeries {
            labels,
            samples: vec![RawSample {
                raw_profile,
                id: Uuid::new_v4().to_string(),
            }],
        };
        req.series.push(series);
    }

    req
}

fn labels_for_profile(
    config: &PyroscopeConfig,
    profile_type: String,
    series_tags: &[Tag],
) -> Vec<LabelPair> {
    let mut labels: Vec<LabelPair> = Vec::with_capacity(6 + config.tags.len() + series_tags.len());
    labels.push(LabelPair {
        name: LABEL_PROFILE_NAME.to_string(),
        value: profile_type,
    });
    for tag in series_tags {
        if tag.key != LABEL_PROFILE_NAME {
            push_label_if_absent(&mut labels, &tag.key, &tag.value);
        }
    }
    for (k, v) in &config.tags {
        if k == LABEL_PROFILE_NAME {
            continue;
        }
        push_label_if_absent(&mut labels, k, v);
    }
    push_label_if_absent(&mut labels, LABEL_SERVICE_NAME, &config.application_name);
    push_label_if_absent(&mut labels, LABEL_SCOPE_NAME, SCOPE_NAME);
    push_label_if_absent(&mut labels, LABEL_SCOPE_VERSION, &config.spy_version);
    push_label_if_absent(
        &mut labels,
        LABEL_PROCESS_RUNTIME_NAME,
        &config.runtime_name,
    );
    push_label_if_absent(
        &mut labels,
        LABEL_PROCESS_RUNTIME_VERSION,
        &config.runtime_version,
    );

    labels
}

fn push_label_if_absent(labels: &mut Vec<LabelPair>, name: &str, value: &str) {
    if labels.iter().any(|label| label.name == name) {
        return;
    }
    labels.push(LabelPair {
        name: name.to_string(),
        value: value.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory;
    use std::collections::HashMap;

    fn tags<const N: usize>(tags: [(&str, &str); N]) -> HashMap<String, String> {
        tags.into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn disabled_stack_config() -> crate::stack::Config {
        crate::stack::Config {
            enabled: false,
            oncpu: false,
        }
    }

    fn disabled_memory_config() -> memory::Config {
        memory::Config {
            enabled: false,
            enable_mem_domain: false,
            max_nframe: 0,
            heap_sample_size: 0,
        }
    }

    #[cfg(not(miri))]
    #[test]
    fn signal_pid_is_an_optional_label_on_its_push_series() {
        use crate::{
            backend::BackendConfig,
            encode::r#gen::google::Profile,
            pyspy_backend::Reporter,
            signal_backend::{ProfileBuffer, tests::profile_frames},
        };
        use pyroscope_python_unwind::METRICS;
        use std::{
            io::Read,
            sync::{Arc, Mutex, atomic::Ordering},
            time::UNIX_EPOCH,
        };

        let _guard = crate::signal_sampler::METRICS_LOCK.lock().unwrap();
        METRICS.take_drop_counts();
        let config = PyroscopeConfig::new(
            "http://localhost:4040",
            "my-app",
            100,
            "signal",
            "1.0.12",
            disabled_memory_config(),
            disabled_stack_config(),
        )
        .tags(tags([("env", "prod")]))
        .runtime("cpython".into(), "3.14".into());
        let range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(1)).unwrap();
        let pid = std::process::id().to_string();
        for report_pid in [false, true] {
            let reporter = Reporter::new_signal(
                Arc::new(Mutex::new(ProfileBuffer::new())),
                BackendConfig {
                    report_pid,
                    ..BackendConfig::default()
                },
            );
            for count in [3, 2] {
                METRICS.resolve_error.fetch_add(count, Ordering::Relaxed);
                let report = reporter.report(100, &range).unwrap();
                let ReportData::RawPprof(payload) = &report.data else {
                    panic!("expected raw pprof");
                };
                // The transport treats raw payloads as opaque; reuse it to test per-series scoping.
                let other = ReportBatch {
                    profile_type: "memory".into(),
                    data: ReportData::RawPprof(payload.clone()),
                    series_tags: Vec::new(),
                };
                let request = build_push_request(&config, &range, vec![report, other]);
                let compressed = Session::gzip(&request.encode_to_vec()).unwrap();
                let mut decoded = Vec::new();
                libflate::gzip::Decoder::new(compressed.as_slice())
                    .unwrap()
                    .read_to_end(&mut decoded)
                    .unwrap();
                let request = PushRequest::decode(decoded.as_slice()).unwrap();
                assert_eq!(request.series.len(), 2, "{request:#?}");
                for (index, series) in request.series.iter().enumerate() {
                    let profile_type = if index == 0 { "process_cpu" } else { "memory" };
                    let mut expected = vec![(LABEL_PROFILE_NAME, profile_type)];
                    if index == 0 && report_pid {
                        expected.push(("pid", pid.as_str()));
                    }
                    expected.extend([
                        ("env", "prod"),
                        (LABEL_SERVICE_NAME, "my-app"),
                        (LABEL_SCOPE_NAME, SCOPE_NAME),
                        (LABEL_SCOPE_VERSION, "1.0.12"),
                        (LABEL_PROCESS_RUNTIME_NAME, "cpython"),
                        (LABEL_PROCESS_RUNTIME_VERSION, "3.14"),
                    ]);
                    let actual: Vec<_> = series
                        .labels
                        .iter()
                        .map(|label| (label.name.as_str(), label.value.as_str()))
                        .collect();
                    assert_eq!(actual, expected, "{request:#?}");
                    assert_eq!(series.samples.len(), 1, "{request:#?}");
                    let profile =
                        Profile::decode(series.samples[0].raw_profile.as_slice()).unwrap();
                    assert_eq!(profile.sample.len(), 1, "{profile:#?}");
                    let sample = &profile.sample[0];
                    assert_eq!(
                        profile_frames(&profile, sample),
                        [("[dropped: resolve_error]", "", 0)],
                        "{profile:#?}"
                    );
                    assert_eq!(sample.value, [count as i64 * 10_000_000], "{profile:#?}");
                    assert!(sample.label.is_empty(), "{profile:#?}");
                }
                assert!(reporter.report(100, &range).unwrap().is_empty());
            }
        }
    }

    #[test]
    fn series_pid_overrides_a_user_tag_without_duplicate_labels() {
        let config = PyroscopeConfig::new(
            "http://localhost:4040",
            "my-app",
            100,
            "signal",
            "1.0.12",
            disabled_memory_config(),
            disabled_stack_config(),
        )
        .tags(tags([
            ("pid", "user-pid"),
            (LABEL_PROFILE_NAME, "user-profile"),
        ]));
        let series_tags = [
            Tag::new("pid".into(), "123".into()),
            Tag::new(LABEL_PROFILE_NAME.into(), "wrong-profile".into()),
        ];
        let labels = labels_for_profile(&config, "process_cpu".into(), &series_tags);
        let pid: Vec<_> = labels
            .iter()
            .filter(|label| label.name == "pid")
            .map(|label| label.value.as_str())
            .collect();
        assert_eq!(pid, ["123"]);
        assert_eq!(labels[0].value, "process_cpu");
        let labels = labels_for_profile(&config, "memory".into(), &[]);
        let pid: Vec<_> = labels
            .iter()
            .filter(|label| label.name == "pid")
            .map(|label| label.value.as_str())
            .collect();
        assert_eq!(pid, ["user-pid"]);
    }

    #[test]
    fn labels_for_profile_includes_scope_and_runtime_labels() {
        let config = PyroscopeConfig::new(
            "http://localhost:4040",
            "my-app",
            100,
            "pyspy",
            "1.0.12",
            disabled_memory_config(),
            disabled_stack_config(),
        )
        .tags(tags([("env", "prod")]))
        .runtime("cpython".to_string(), "3.12.4".to_string());

        let labels = labels_for_profile(&config, "process_cpu".to_string(), &[]);
        let labels_by_name: HashMap<&str, &str> = labels
            .iter()
            .map(|label| (label.name.as_str(), label.value.as_str()))
            .collect();

        assert_eq!(labels_by_name.get(LABEL_SERVICE_NAME), Some(&"my-app"));
        assert_eq!(labels_by_name.get(LABEL_PROFILE_NAME), Some(&"process_cpu"));
        assert_eq!(
            labels.first().map(|label| label.name.as_str()),
            Some(LABEL_PROFILE_NAME)
        );
        assert_eq!(labels_by_name.get("env"), Some(&"prod"));
        assert_eq!(labels_by_name.get(LABEL_SCOPE_NAME), Some(&SCOPE_NAME));
        assert_eq!(labels_by_name.get(LABEL_SCOPE_VERSION), Some(&"1.0.12"));
        assert_eq!(
            labels_by_name.get(LABEL_PROCESS_RUNTIME_NAME),
            Some(&"cpython")
        );
        assert_eq!(
            labels_by_name.get(LABEL_PROCESS_RUNTIME_VERSION),
            Some(&"3.12.4")
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.name == LABEL_SCOPE_NAME)
                .count(),
            1
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.name == LABEL_PROCESS_RUNTIME_VERSION)
                .count(),
            1
        );
    }

    #[test]
    fn labels_for_profile_preserves_user_provided_semconv_labels() {
        let config = PyroscopeConfig::new(
            "http://localhost:4040",
            "my-app",
            100,
            "pyspy",
            "1.0.12",
            disabled_memory_config(),
            disabled_stack_config(),
        )
        .tags(tags([
            (LABEL_SCOPE_NAME, "user-supplied-scope"),
            (LABEL_SCOPE_VERSION, "user-supplied-scope-version"),
            (LABEL_PROCESS_RUNTIME_NAME, "user-supplied-runtime"),
            (
                LABEL_PROCESS_RUNTIME_VERSION,
                "user-supplied-runtime-version",
            ),
        ]))
        .runtime("cpython".to_string(), "3.12.4".to_string());

        let labels = labels_for_profile(&config, "process_cpu".to_string(), &[]);
        let labels_by_name: HashMap<&str, &str> = labels
            .iter()
            .map(|label| (label.name.as_str(), label.value.as_str()))
            .collect();

        assert_eq!(
            labels_by_name.get(LABEL_SCOPE_NAME),
            Some(&"user-supplied-scope")
        );
        assert_eq!(
            labels_by_name.get(LABEL_SCOPE_VERSION),
            Some(&"user-supplied-scope-version")
        );
        assert_eq!(
            labels_by_name.get(LABEL_PROCESS_RUNTIME_NAME),
            Some(&"user-supplied-runtime")
        );
        assert_eq!(
            labels_by_name.get(LABEL_PROCESS_RUNTIME_VERSION),
            Some(&"user-supplied-runtime-version")
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.name == LABEL_SCOPE_NAME)
                .count(),
            1
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.name == LABEL_PROCESS_RUNTIME_VERSION)
                .count(),
            1
        );
    }

    #[test]
    fn labels_for_profile_uses_user_service_name_and_ignores_user_profile_name() {
        let config = PyroscopeConfig::new(
            "http://localhost:4040",
            "my-app",
            100,
            "pyspy",
            "1.0.12",
            disabled_memory_config(),
            disabled_stack_config(),
        )
        .tags(tags([
            (LABEL_SERVICE_NAME, "user-service"),
            (LABEL_PROFILE_NAME, "user-profile"),
        ]))
        .runtime("cpython".to_string(), "3.12.4".to_string());

        let labels = labels_for_profile(&config, "process_cpu".to_string(), &[]);
        let labels_by_name: HashMap<&str, &str> = labels
            .iter()
            .map(|label| (label.name.as_str(), label.value.as_str()))
            .collect();

        assert_eq!(
            labels_by_name.get(LABEL_SERVICE_NAME),
            Some(&"user-service")
        );
        assert_eq!(labels_by_name.get(LABEL_PROFILE_NAME), Some(&"process_cpu"));
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.name == LABEL_SERVICE_NAME)
                .count(),
            1
        );
        assert_eq!(
            labels
                .iter()
                .filter(|label| label.name == LABEL_PROFILE_NAME)
                .count(),
            1
        );
    }
}
