use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::{
        Arc,
        mpsc::{self, Sender},
    },
    thread::JoinHandle,
};

use crate::{
    PyroscopeError,
    backend::{BackendReady, BackendUninitialized, Tag},
    error::Result,
    session::{Session, SessionManager, SessionSignal},
};
use std::sync::Mutex;
use std::sync::mpsc::SyncSender;
use std::time::{Duration, SystemTime};

use crate::backend::{Backend, BackendImpl, ThreadTag, ThreadTagsSet};
use crate::utils::TimeRange;

const LOG_TAG: &str = "Pyroscope::Agent";
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
    // pub mem_config: crate::mem::Config,
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
        // mem_config: mem::Config,
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
            // mem_config,
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
}

/// PyroscopeAgent Builder
///
/// # Example
/// ```no_run
/// use pyroscope::pyroscope::PyroscopeAgentBuilder;
/// use pyroscope::backend::{pprof_backend, PprofConfig, BackendConfig};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let agent = PyroscopeAgentBuilder::new(
///     "http://localhost:8080", "my-app", 100, "pyroscope-rs", "0.1.0",
///     pprof_backend(PprofConfig::default(), BackendConfig::default()),
/// )
/// .build()?;
/// # Ok(())
/// # }
/// ```
pub struct PyroscopeAgentBuilder {
    /// Profiler backend
    backend: BackendImpl<BackendUninitialized>,
    /// Configuration Object
    config: PyroscopeConfig,
    ruleset: ThreadTagsSet,
    http_client: reqwest::blocking::Client,
}

impl PyroscopeAgentBuilder {
    pub fn new(
        config: PyroscopeConfig,
        backend: BackendImpl<BackendUninitialized>,
        ruleset: ThreadTagsSet,
        http_client: reqwest::blocking::Client,
    ) -> Self {
        Self {
            backend,
            config,
            ruleset,
            http_client,
        }
    }

    pub fn build(self) -> Result<PyroscopeAgent<PyroscopeAgentReady>> {
        let config = self.config;

        // Set Global Tags
        // for (key, value) in config.tags.iter() {
        // todo!("implement")
        // }

        // Initialize the Backend
        let backend_ready = self.backend.initialize()?;
        log::trace!(target: LOG_TAG, "Backend initialized");

        let session_manager = SessionManager::new(self.http_client);
        log::trace!(target: LOG_TAG, "SessionManager initialized");

        // Return PyroscopeAgent
        Ok(PyroscopeAgent {
            backend: backend_ready,
            config,
            session_manager,
            terminate_channel: None,
            handle: None,
            _state: PhantomData,
            ruleset: self.ruleset,
        })
    }
}

/// This trait is used to encode the state of the agent.
pub trait PyroscopeAgentState {}

/// Marker struct for an Uninitialized state.
#[derive(Debug)]
pub struct PyroscopeAgentBare;

/// Marker struct for a Ready state.
#[derive(Debug)]
pub struct PyroscopeAgentReady;

/// Marker struct for a Running state.
#[derive(Debug)]
pub struct PyroscopeAgentRunning;

impl PyroscopeAgentState for PyroscopeAgentBare {}

impl PyroscopeAgentState for PyroscopeAgentReady {}

impl PyroscopeAgentState for PyroscopeAgentRunning {}

pub struct PyroscopeAgent<S: PyroscopeAgentState> {
    session_manager: SessionManager,
    terminate_channel: Option<Sender<()>>,
    /// Handle to the thread that runs the Pyroscope Agent
    handle: Option<JoinHandle<Result<()>>>,
    /// Profiler backend
    pub backend: BackendImpl<BackendReady>,
    /// Configuration Object
    pub config: PyroscopeConfig,
    /// PyroscopeAgent State
    _state: PhantomData<S>,

    ruleset: ThreadTagsSet,
}

impl<S: PyroscopeAgentState> PyroscopeAgent<S> {
    /// Transition the PyroscopeAgent to a new state.
    fn transition<D: PyroscopeAgentState>(self) -> PyroscopeAgent<D> {
        PyroscopeAgent {
            session_manager: self.session_manager,
            terminate_channel: self.terminate_channel,
            handle: self.handle,
            backend: self.backend,
            config: self.config,
            _state: PhantomData,
            ruleset: self.ruleset,
        }
    }
}

impl<S: PyroscopeAgentState> PyroscopeAgent<S> {
    fn shutdown(mut self) {
        log::debug!(target: LOG_TAG, "PyroscopeAgent::drop()");

        match self.backend.shutdown() {
            Ok(_) => log::debug!(target: LOG_TAG, "Backend shutdown"),
            Err(e) => log::error!(target: LOG_TAG, "Backend shutdown error: {e}"),
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

impl PyroscopeAgent<PyroscopeAgentReady> {
    pub fn start(mut self) -> Result<PyroscopeAgent<PyroscopeAgentRunning>> {
        log::debug!(target: LOG_TAG, "Starting");

        // Create a clone of Backend
        let backend = Arc::clone(&self.backend.backend);
        // Call start()

        let (tx, rx) = mpsc::channel();
        self.terminate_channel = Some(tx);

        let config = self.config.clone();

        // Clone SessionManager Sender
        let stx = self.session_manager.tx.clone();

        self.handle = Some(std::thread::spawn(move || {
            log::trace!(target: LOG_TAG, "Main Thread started");
            let mut sw = StopWatch::new();
            loop {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        Self::snapshot(&backend, config.clone(), &stx, &mut sw)?;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        Self::snapshot(&backend, config.clone(), &stx, &mut sw)?;
                        log::trace!(target: LOG_TAG, "Session Killed");
                        break Ok(());
                    }
                    Ok(_) => {
                        // unreachable: nothing is ever sent;
                    }
                }
            }
        }));

        Ok(self.transition())
    }

    fn snapshot(
        backend: &Arc<Mutex<Option<Box<dyn Backend>>>>,
        config: PyroscopeConfig,
        stx: &SyncSender<SessionSignal>,
        stop_watch: &mut StopWatch,
    ) -> Result<()> {
        let time_range = stop_watch.lap()?;

        let mut batch = Vec::with_capacity(2);

        // if let Some(pprof) = mem::dump_pprof(config.mem_config.heap_sample_size, &time_range) {
        //     batch.push(ReportBatch{
        //         profile_type: "memory".to_string(),
        //         data: ReportData::RawPprof(pprof),
        //     })
        // }
        log::trace!(target: LOG_TAG, "Sending session {:?}",  time_range);

        // Generate report from backend
        let report = backend
            .lock()?
            .as_mut()
            .ok_or_else(|| {
                PyroscopeError::AdHoc("PyroscopeAgent - Failed to unwrap backend".to_string())
            })?
            .report()?;

        batch.push(report);

        // Send new Session to SessionManager
        stx.send(SessionSignal::Session(Box::new(Session::new(
            time_range, config, batch,
        ))))?;
        Ok(())
    }
}

impl PyroscopeAgent<PyroscopeAgentRunning> {
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
