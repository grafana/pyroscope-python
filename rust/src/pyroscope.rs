use std::{
    collections::HashMap,
    sync::mpsc::{self, Sender},
    thread::JoinHandle,
};

use crate::{
    PyroscopeError,
    backend::Tag,
    error::Result,
    session::{Session, SessionManager, SessionSignal},
    timer::{Timer, TimerSignal},
    utils::get_time_range,
};

use crate::backend::{BackendConfig, ThreadTag, ThreadTagsSet};
use crate::pyspy_backend::Pyspy;

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

    pub fn tags(self, tags: Vec<(&str, &str)>) -> Self {
        // Convert &[(&str, &str)] to HashMap(String, String)
        let tags_hashmap: HashMap<String, String> = tags
            .to_owned()
            .iter()
            .cloned()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect();

        Self {
            tags: tags_hashmap,
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

pub struct PyroscopeAgentBuilder {
    config: PyroscopeConfig,
    pyspy_config: py_spy::config::Config,
    backend_config: BackendConfig,
    ruleset: ThreadTagsSet,
}

impl PyroscopeAgentBuilder {
    pub fn new(
        config: PyroscopeConfig,
        pyspy_config: py_spy::config::Config,
        backend_config: BackendConfig,
        ruleset: ThreadTagsSet,
    ) -> Self {
        Self {
            config,
            pyspy_config,
            backend_config,
            ruleset,
        }
    }

    pub fn build(self) -> Result<PyroscopeAgent> {
        let config = self.config;

        let backend = Pyspy::new(self.pyspy_config, self.backend_config, self.ruleset.clone())?;

        log::trace!(target: LOG_TAG, "Backend initialized");

        // Start the Timer
        let timer = Timer::initialize(std::time::Duration::from_secs(10))?;
        log::trace!(target: LOG_TAG, "Timer initialized");

        // Start the SessionManager
        let session_manager = SessionManager::new()?;
        log::trace!(target: LOG_TAG, "SessionManager initialized");

        PyroscopeAgent {
            backend,
            config,
            timer,
            session_manager,
            tx: None,
            handle: None,
            ruleset: self.ruleset,
        }
        .start()
    }
}

/// PyroscopeAgent is the main object of the library. It is used to start and stop the profiler, schedule the timer, and send the profiler data to the server.
pub struct PyroscopeAgent {
    /// Instance of the Timer
    timer: Timer,
    /// Instance of the SessionManager
    session_manager: SessionManager,
    /// Channel sender for the timer thread
    tx: Option<Sender<TimerSignal>>,
    /// Handle to the thread that runs the Pyroscope Agent
    handle: Option<JoinHandle<Result<()>>>,
    /// Profiler backend
    pub backend: Pyspy,
    /// Configuration Object
    pub config: PyroscopeConfig,

    ruleset: ThreadTagsSet,
}

impl PyroscopeAgent {
    /// Properly shutdown the agent.
    pub fn shutdown(mut self) {
        log::debug!(target: LOG_TAG, "PyroscopeAgent::drop()");

        // Shutdown Backend
        match self.backend.shutdown() {
            Ok(_) => log::debug!(target: LOG_TAG, "Backend shutdown"),
            Err(e) => log::error!(target: LOG_TAG, "Backend shutdown error: {e}"),
        }

        // Drop Timer listeners
        match self.timer.drop_listeners() {
            Ok(_) => log::trace!(target: LOG_TAG, "Dropped timer listeners"),
            Err(_) => log::error!(target: LOG_TAG, "Error Dropping timer listeners"),
        }

        // Wait for the Timer thread to finish
        if let Some(handle) = self.timer.handle.take() {
            match handle.join() {
                Ok(_) => log::trace!(target: LOG_TAG, "Dropped timer thread"),
                Err(_) => log::error!(target: LOG_TAG, "Error Dropping timer thread"),
            }
        }

        // Stop the SessionManager
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

        // Wait for main thread to finish
        if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(_) => log::trace!(target: LOG_TAG, "Dropped main thread"),
                Err(_) => log::error!(target: LOG_TAG, "Error Dropping main thread"),
            }
        }

        log::debug!(target: LOG_TAG, "Agent Shutdown");
    }
}

impl PyroscopeAgent {
    fn start(mut self) -> Result<PyroscopeAgent> {
        log::debug!(target: LOG_TAG, "Starting");

        // Create a clone of Backend
        let reporter = self.backend.reporter();
        // Call start()

        // Create a channel to listen for timer signals
        let (tx, rx) = mpsc::channel();
        self.timer.attach_listener(tx.clone())?;
        self.tx = Some(tx);

        let config = self.config.clone();

        // Clone SessionManager Sender
        let stx = self.session_manager.tx.clone();

        self.handle = Some(std::thread::spawn(move || {
            log::trace!(target: LOG_TAG, "Main Thread started");

            while let Ok(signal) = rx.recv() {
                match signal {
                    TimerSignal::NextSnapshot(until) => {
                        // get_time_range should be used with "from". We balance this by reducing
                        // 10s from the returned range.
                        let mut time_range = get_time_range(until)?;
                        time_range.from -= 10;
                        time_range.until -= 10;

                        let mut batch = Vec::with_capacity(2);

                        // if let Some(pprof) = mem::dump_pprof(config.mem_config.heap_sample_size, &time_range) {
                        //     batch.push(ReportBatch{
                        //         profile_type: "memory".to_string(),
                        //         data: ReportData::RawPprof(pprof),
                        //     })
                        // }

                        log::trace!(target: LOG_TAG, "Sending session {until}");

                        let report = reporter.report()?;

                        batch.push(report);

                        // Send new Session to SessionManager
                        stx.send(SessionSignal::Session(Box::new(Session::new(
                            time_range,
                            config.clone(),
                            batch,
                        ))))?
                    }
                    TimerSignal::Terminate => {
                        log::trace!(target: LOG_TAG, "Session Killed");
                        // mem::stop();

                        return Ok(());
                    }
                }
            }
            Ok(())
        }));

        Ok(self)
    }
}

impl PyroscopeAgent {
    pub fn stop(mut self) -> Result<()> {
        log::debug!(target: LOG_TAG, "Stopping");
        // get tx and send termination signal
        if let Some(sender) = self.tx.take() {
            // Send last session
            sender.send(TimerSignal::NextSnapshot(get_time_range(0)?.until))?;
            // Terminate PyroscopeAgent internal thread
            sender.send(TimerSignal::Terminate)?;
        } else {
            log::error!("PyroscopeAgent - Missing sender")
        }

        self.shutdown();
        Ok(())
    }

    /// Add a thread Tag rule to the backend Ruleset. For tagging, it's
    /// recommended to use the `tag_wrapper` function.
    pub fn add_thread_tag(&self, thread_id: crate::utils::ThreadId, tag: Tag) -> Result<()> {
        let rule = ThreadTag::new(thread_id, tag);
        self.ruleset.add(rule)?;

        Ok(())
    }

    /// Remove a thread Tag rule from the backend Ruleset. For tagging, it's
    /// recommended to use the `tag_wrapper` function.
    pub fn remove_thread_tag(&self, thread_id: crate::utils::ThreadId, tag: Tag) -> Result<()> {
        let rule = ThreadTag::new(thread_id, tag);
        self.ruleset.remove(rule)?;

        Ok(())
    }
}

pub fn parse_http_headers_json(http_headers_json: String) -> Result<HashMap<String, String>> {
    let mut http_headers = HashMap::new();
    let parsed: serde_json::Value = serde_json::from_str(&http_headers_json)?;
    let parsed = parsed
        .as_object()
        .ok_or_else(|| PyroscopeError::AdHoc(format!("expected object, got {parsed}")))?;
    for (k, v) in parsed {
        if let Some(value) = v.as_str() {
            http_headers.insert(k.to_string(), value.to_string());
        } else {
            return Err(PyroscopeError::AdHoc(format!(
                "invalid http header value, not a string: {v}"
            )));
        }
    }
    Ok(http_headers)
}

pub fn parse_vec_string_json(s: String) -> Result<Vec<String>> {
    let parsed: serde_json::Value = serde_json::from_str(&s)?;
    let parsed = parsed
        .as_array()
        .ok_or_else(|| PyroscopeError::AdHoc(format!("expected array, got {parsed}")))?;
    let mut res = Vec::with_capacity(parsed.len());
    for v in parsed {
        if let Some(s) = v.as_str() {
            res.push(s.to_string());
        } else {
            return Err(PyroscopeError::AdHoc(format!(
                "invalid element value, not a string: {v}"
            )));
        }
    }
    Ok(res)
}
