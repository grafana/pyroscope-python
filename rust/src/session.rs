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
    backend::{ReportBatch, ReportData},
    encode::pprof,
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

        let mut req = PushRequest {
            series: Vec::with_capacity(self.batch.len()),
        };
        for batch in self.batch {
            let ReportBatch { profile_type, data } = batch;

            let raw_profile = match data {
                ReportData::RawPprof(pprof_bytes) => pprof_bytes,
                ReportData::Reports(reports) => {
                    pprof::encode(reports, self.config.sample_rate, self.time_range.clone())
                        .encode_to_vec()
                }
            };

            let labels = labels_for_profile(&self.config, profile_type);
            let series = RawProfileSeries {
                labels,
                samples: vec![RawSample {
                    raw_profile,
                    id: Uuid::new_v4().to_string(),
                }],
            };
            req.series.push(series);
        }

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

fn labels_for_profile(config: &PyroscopeConfig, profile_type: String) -> Vec<LabelPair> {
    let mut labels: Vec<LabelPair> = Vec::with_capacity(6 + config.tags.len());
    labels.push(LabelPair {
        name: LABEL_PROFILE_NAME.to_string(),
        value: profile_type,
    });
    for (k, v) in &config.tags {
        if k == LABEL_PROFILE_NAME {
            continue;
        }
        labels.push(LabelPair {
            name: k.clone(),
            value: v.clone(),
        })
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
    use std::collections::HashMap;

    fn tags<const N: usize>(tags: [(&str, &str); N]) -> HashMap<String, String> {
        tags.into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn labels_for_profile_includes_scope_and_runtime_labels() {
        let config =
            PyroscopeConfig::new("http://localhost:4040", "my-app", 100, "pyspy", "1.0.12")
                .tags(tags([("env", "prod")]))
                .runtime("cpython".to_string(), "3.12.4".to_string());

        let labels = labels_for_profile(&config, "process_cpu".to_string());
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
        let config =
            PyroscopeConfig::new("http://localhost:4040", "my-app", 100, "pyspy", "1.0.12")
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

        let labels = labels_for_profile(&config, "process_cpu".to_string());
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
        let config =
            PyroscopeConfig::new("http://localhost:4040", "my-app", 100, "pyspy", "1.0.12")
                .tags(tags([
                    (LABEL_SERVICE_NAME, "user-service"),
                    (LABEL_PROFILE_NAME, "user-profile"),
                ]))
                .runtime("cpython".to_string(), "3.12.4".to_string());

        let labels = labels_for_profile(&config, "process_cpu".to_string());
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
