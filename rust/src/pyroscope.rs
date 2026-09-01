use std::{
    collections::HashMap,
    sync::mpsc::{self, Sender},
    thread::JoinHandle,
};

use crate::{
    backend::{ReportBatch, ReportData, Tag, ThreadTag, ThreadTagsSet},
    error::Result,
    memory,
    session::{Session, SessionManager, SessionSignal},
};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, SystemTime};

use crate::cpu::{CpuBackend, CpuConfig, CpuReporter};
use crate::utils::TimeRange;
const LOG_TAG: &str = "Pyroscope::Agent";
const DEFAULT_UPLOAD_INTERVAL: Duration = Duration::from_secs(10);
#[derive(Clone)]
pub struct PyroscopeConfig {
    /// Pyroscope Server Address
    pub url: String,
    /// Application Name
    pub application_name: String,
    /// Tags
    pub tags: HashMap<String, String>,
    /// Sample Rate
    pub sample_rate: u32,
    /// Spy Name
    pub spy_name: String,
    /// Spy Version
    pub spy_version: String,
    /// Runtime Name
    pub runtime_name: String,
    /// Runtime Version
    pub runtime_version: String,
    pub basic_auth: Option<BasicAuth>,
    pub tenant_id: Option<String>,
    pub http_headers: HashMap<String, String>,
    /// How often the agent snapshots and uploads profile data.
    pub upload_interval: Duration,
    pub mem_config: crate::memory::Config,
}

#[derive(Clone, Debug)]
pub struct BasicAuth {
    pub username: String,
    pub password: String,
}

impl PyroscopeConfig {
    pub fn new(
        url: impl AsRef<str>,
        application_name: impl AsRef<str>,
        sample_rate: u32,
        spy_name: impl AsRef<str>,
        spy_version: impl AsRef<str>,
        mem_config: crate::memory::Config,
    ) -> Self {
        Self {
            url: url.as_ref().to_owned(),
            application_name: application_name.as_ref().to_owned(),
            tags: HashMap::new(),
            sample_rate,
            spy_name: spy_name.as_ref().to_owned(),
            spy_version: spy_version.as_ref().to_owned(),
            runtime_name: String::new(),
            runtime_version: String::new(),
            basic_auth: None,
            tenant_id: None,
            http_headers: HashMap::new(),
            upload_interval: DEFAULT_UPLOAD_INTERVAL,
            mem_config,
        }
    }

    // Set the Pyroscope Server URL
    pub fn url(self, url: impl AsRef<str>) -> Self {
        Self {
            url: url.as_ref().to_owned(),
            ..self
        }
    }

    pub fn basic_auth(self, username: String, password: String) -> Self {
        Self {
            basic_auth: Some(BasicAuth { username, password }),
            ..self
        }
    }

    pub fn tags(self, tags: HashMap<String, String>) -> Self {
        Self { tags, ..self }
    }

    pub fn runtime(self, runtime_name: String, runtime_version: String) -> Self {
        Self {
            runtime_name,
            runtime_version,
            ..self
        }
    }

    pub fn tenant_id(self, tenant_id: String) -> Self {
        Self {
            tenant_id: Some(tenant_id),
            ..self
        }
    }

    pub fn http_headers(self, http_headers: HashMap<String, String>) -> Self {
        Self {
            http_headers,
            ..self
        }
    }

    pub fn upload_interval(self, upload_interval: Duration) -> Self {
        Self {
            upload_interval,
            ..self
        }
    }
}

pub struct PyroscopeAgentBuilder {
    pub config: PyroscopeConfig,
    /// `None` when CPU profiling is disabled.
    cpu_config: Option<CpuConfig>,
    ruleset: ThreadTagsSet,
}

impl PyroscopeAgentBuilder {
    pub fn new(
        config: PyroscopeConfig,
        cpu_config: Option<CpuConfig>,
        ruleset: ThreadTagsSet,
    ) -> Self {
        Self {
            config,
            cpu_config,
            ruleset,
        }
    }

    pub fn build(self, http_client: reqwest::blocking::Client) -> Result<PyroscopeAgent> {
        let config = self.config;

        // Set Global Tags
        // for (key, value) in config.tags.iter() {
        // todo!("implement")
        // }

        let backend = self
            .cpu_config
            .map(|cpu_config| CpuBackend::new(cpu_config, self.ruleset.clone()))
            .transpose()?;
        if backend.is_some() {
            log::trace!(target: LOG_TAG, "Backend initialized");
        }

        let session_manager = SessionManager::new(http_client);
        log::trace!(target: LOG_TAG, "SessionManager initialized");

        // Return PyroscopeAgent
        Ok(PyroscopeAgent {
            backend,
            config,
            session_manager,
            terminate_channel: None,
            handle: None,
            ruleset: self.ruleset,
        })
    }
}

pub struct PyroscopeAgent {
    session_manager: SessionManager,
    terminate_channel: Option<Sender<()>>,
    /// Handle to the thread that runs the Pyroscope Agent
    handle: Option<JoinHandle<Result<()>>>,
    /// CPU profiler backend
    pub backend: Option<CpuBackend>,
    /// Configuration Object
    pub config: PyroscopeConfig,

    ruleset: ThreadTagsSet,
}

impl PyroscopeAgent {
    fn shutdown(mut self) {
        log::debug!(target: LOG_TAG, "PyroscopeAgent::drop()");

        if let Some(backend) = &mut self.backend {
            match backend.shutdown_thread() {
                Ok(_) => log::debug!(target: LOG_TAG, "Backend shutdown"),
                Err(e) => log::error!(target: LOG_TAG, "Backend shutdown error: {e}"),
            }
        }

        match self.session_manager.push(SessionSignal::Kill) {
            Ok(_) => log::trace!(target: LOG_TAG, "Sent kill signal to SessionManager"),
            Err(_) => log::error!(
                target: LOG_TAG,
                "Error sending kill signal to SessionManager"
            ),
        }

        if let Some(handle) = self.session_manager.handle.take() {
            match handle.join() {
                Ok(_) => log::trace!(target: LOG_TAG, "Dropped SessionManager thread"),
                Err(_) => log::error!(target: LOG_TAG, "Error Dropping SessionManager thread"),
            }
        }

        log::debug!(target: LOG_TAG, "Agent Shutdown");
    }
}

impl PyroscopeAgent {
    pub fn start(mut self) -> Result<PyroscopeAgent> {
        log::debug!(target: LOG_TAG, "Starting");

        let reporter = self.backend.as_ref().map(CpuBackend::reporter);

        let (tx, rx) = mpsc::channel();
        self.terminate_channel = Some(tx);

        let config = self.config.clone();

        // Clone SessionManager Sender
        let stx = self.session_manager.tx.clone();

        self.handle = Some(std::thread::spawn(move || {
            log::trace!(target: LOG_TAG, "Main Thread started");
            let mut sw = StopWatch::new();
            loop {
                match rx.recv_timeout(config.upload_interval) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        Self::snapshot(&reporter, config.clone(), &stx, &mut sw)?;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        Self::snapshot(&reporter, config.clone(), &stx, &mut sw)?;
                        log::trace!(target: LOG_TAG, "Session Killed");
                        break Ok(());
                    }
                    Ok(_) => {
                        // unreachable: nothing is ever sent;
                    }
                }
            }
        }));

        Ok(self)
    }

    fn snapshot(
        reporter: &Option<CpuReporter>,
        config: PyroscopeConfig,
        stx: &SyncSender<SessionSignal>,
        stop_watch: &mut StopWatch,
    ) -> Result<()> {
        let time_range = stop_watch.lap()?;

        let mut batch = Vec::with_capacity(2);

        if config.mem_config.enabled {
            let pprof = memory::dump_pprof(config.mem_config.heap_sample_size, &time_range);
            if let Some(pprof) = pprof {
                batch.push(ReportBatch {
                    profile_type: "memory".to_string(),
                    data: ReportData::RawPprof(pprof),
                })
            }
        }
        log::trace!(target: LOG_TAG, "Sending session {:?}",  time_range);

        if let Some(reporter) = reporter {
            let report = reporter.report()?;
            batch.push(report);
        }

        // Send new Session to SessionManager
        stx.send(SessionSignal::Session(Box::new(Session::new(
            time_range, config, batch,
        ))))?;
        Ok(())
    }
}

impl PyroscopeAgent {
    pub fn stop(mut self) -> Result<()> {
        log::debug!(target: LOG_TAG, "Stopping");

        drop(self.terminate_channel.take());
        if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(Ok(_)) => log::trace!(target: LOG_TAG, "Main thread exited"),
                Ok(Err(err)) => {
                    log::error!(target: LOG_TAG, "Main thread exited early {}", err)
                }
                Err(err) => log::error!(target: LOG_TAG, "Error Dropping main thread: {:?}", err),
            }
        }

        self.shutdown();
        Ok(())
    }

    /// Add a thread Tag rule to the agent Ruleset.
    pub fn add_thread_tag(&self, thread_id: crate::utils::ThreadId, tag: Tag) -> Result<()> {
        let rule = ThreadTag::new(thread_id, tag);
        self.ruleset.add(rule)?;

        Ok(())
    }

    /// Remove a thread Tag rule from the agent Ruleset.
    pub fn remove_thread_tag(&self, thread_id: crate::utils::ThreadId, tag: Tag) -> Result<()> {
        let rule = ThreadTag::new(thread_id, tag);
        self.ruleset.remove(rule)?;

        Ok(())
    }
}

struct StopWatch {
    start: SystemTime,
}

impl StopWatch {
    pub fn new() -> StopWatch {
        Self {
            start: SystemTime::now(),
        }
    }

    pub fn lap(&mut self) -> Result<TimeRange> {
        let until = SystemTime::now();
        let res = TimeRange::new(self.start, until)?;
        self.start = until;
        Ok(res)
    }
}
