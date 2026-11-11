use crate::{
    backend::{
        BackendConfig, Report, ReportBatch, ReportData, StackBuffer, StackFrame, StackTrace,
        ThreadTagsSet,
    },
    error::{PyroscopeError, Result},
    utils::TimeRange,
};
use prost::Message;
use py_spy::sampler::Sampler;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

pub struct Pyspy {
    buffer: Arc<Mutex<StackBuffer>>,
    config: py_spy::config::Config,
    backend_config: BackendConfig,
    sampler_thread: Option<JoinHandle<Result<()>>>,
    running: Arc<AtomicBool>,
    ruleset: ThreadTagsSet,
}

pub enum Reporter {
    PySpy(Arc<Mutex<StackBuffer>>),
    Signal(Arc<Mutex<crate::signal_backend::ProfileBuffer>>),
}

impl Pyspy {
    pub fn new(
        config: py_spy::config::Config,
        backend_config: BackendConfig,
        ruleset: ThreadTagsSet,
    ) -> Result<Self> {
        let mut res = Pyspy {
            buffer: Arc::new(Mutex::new(StackBuffer::default())),
            config,
            backend_config,
            sampler_thread: None,
            running: Arc::new(AtomicBool::new(false)),
            ruleset,
        };
        res.initialize()?;
        Ok(res)
    }

    pub fn reporter(&self) -> Reporter {
        Reporter::PySpy(self.buffer.clone())
    }
}

impl Pyspy {
    fn initialize(&mut self) -> Result<()> {
        if self.config.pid.is_none() {
            return Err(PyroscopeError::new("Pyspy: No Process ID Specified"));
        }

        let running = Arc::clone(&self.running);
        running.store(true, Ordering::Relaxed);

        let buffer = self.buffer.clone();

        let config = self.config.clone();

        let ruleset = self.ruleset.clone();

        let backend_config = self.backend_config;

        self.sampler_thread = Some(std::thread::spawn(move || {
            let pid = config
                .pid
                .ok_or_else(|| PyroscopeError::new("Pyspy: PID is not set"))?;

            let sampler = Sampler::new(pid, &config)
                .map_err(|e| PyroscopeError::new(&format!("Pyspy: Sampler Error: {}", e)))?;

            let sampler_output = sampler.take_while(|_x| running.load(Ordering::Relaxed));

            for sample in sampler_output {
                for trace in sample.traces {
                    if !(config.include_idle || trace.active) {
                        continue;
                    }

                    if config.gil_only && !trace.owns_gil {
                        continue;
                    }

                    let own_trace: StackTrace =
                        Into::<StackTraceWrapper>::into((trace.clone(), &backend_config)).into();

                    let stacktrace = own_trace.add_tag_rules(&ruleset);

                    buffer.lock()?.record(stacktrace)?;
                }
            }

            Ok(())
        }));

        Ok(())
    }

    pub fn shutdown_thread(&mut self) -> Result<()> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.sampler_thread.take() {
            handle.join().unwrap_or_else(|_| {
                Err(PyroscopeError::new("Pyspy: Failed to join sampler thread"))
            })?
        }
        Ok(())
    }
}

impl Drop for Pyspy {
    fn drop(&mut self) {
        let _ = self.shutdown_thread();
    }
}

impl Reporter {
    pub fn report(&self, sample_rate: u32, time_range: &TimeRange) -> Result<ReportBatch> {
        let data = match self {
            Self::PySpy(buffer) => {
                let report = std::mem::take(&mut *buffer.lock()?);
                let reports: Vec<Report> = report.into();
                ReportData::Reports(reports)
            }
            Self::Signal(buffer) => {
                let profile = buffer.lock()?.report(sample_rate, time_range);
                // Serialize after releasing the lock so the sampler can drain the next batch.
                ReportData::RawPprof(profile.map(|p| p.encode_to_vec()).unwrap_or_default())
            }
        };

        Ok(ReportBatch {
            profile_type: "process_cpu".into(),
            data,
        })
    }
}

/// Wrapper for StackFrame. This is needed because both StackFrame and
/// py_spy::Frame are not defined in the same module.
struct StackFrameWrapper(StackFrame);

impl From<StackFrameWrapper> for StackFrame {
    fn from(stack_frame: StackFrameWrapper) -> Self {
        stack_frame.0
    }
}

impl From<py_spy::Frame> for StackFrameWrapper {
    fn from(frame: py_spy::Frame) -> Self {
        // Format name as "module.function" when module is available,
        // otherwise just use the function name.
        let formatted_name = match frame.module {
            Some(module) => format!("{}.{}", module, frame.name),
            None => frame.name,
        };

        StackFrameWrapper(StackFrame {
            name: formatted_name,
            filename: frame.filename,
            line: frame.line as u32,
        })
    }
}

/// Wrapper for StackTrace. This is needed because both StackTrace and
/// py_spy::StackTrace are not defined in the same module.
struct StackTraceWrapper(StackTrace);

impl From<StackTraceWrapper> for StackTrace {
    fn from(stack_trace: StackTraceWrapper) -> Self {
        stack_trace.0
    }
}

impl From<(py_spy::StackTrace, &BackendConfig)> for StackTraceWrapper {
    fn from(arg: (py_spy::StackTrace, &BackendConfig)) -> Self {
        let (stack_trace, config) = arg;
        // https://github.com/python/cpython/blob/main/Python/thread_pthread.h#L304
        let thread_id = stack_trace.thread_id as libc::pthread_t;
        let stacktrace = StackTrace::new(
            config,
            Some(stack_trace.pid as u32),
            Some(thread_id.into()),
            stack_trace.thread_name.clone(),
            stack_trace
                .frames
                .iter()
                .map(|frame| Into::<StackFrameWrapper>::into(frame.clone()).into())
                .collect(),
        );
        StackTraceWrapper(stacktrace)
    }
}

/// Adapt SDK reports to the transport-independent pprof builder.
pub(crate) fn encode_reports(
    reports: Vec<Report>,
    sample_rate: u32,
    time_range: TimeRange,
) -> pyroscope_pprof::proto::Profile {
    use pyroscope_pprof::{PProfBuilder, PySpyProfile, StringTable};
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_encoder_preserves_complete_stacks_weights_labels_and_time() {
        use crate::backend::Tag;
        use pyroscope_pprof::proto::Profile;
        use std::{
            collections::HashMap,
            time::{Duration, UNIX_EPOCH},
        };

        let stack = |leaf: &str| {
            let mut stack = StackTrace::new(
                &BackendConfig {
                    report_pid: true,
                    ..Default::default()
                },
                Some(42),
                None,
                None,
                vec![
                    StackFrame {
                        name: leaf.into(),
                        filename: "worker_é.py".into(),
                        line: 20,
                    },
                    StackFrame {
                        name: "main".into(),
                        filename: "main.py".into(),
                        line: 10,
                    },
                ],
            );
            stack
                .metadata
                .add_tag(Tag::new("service".into(), "fixture".into()));
            stack
        };
        let range = TimeRange::new(
            UNIX_EPOCH + Duration::from_secs(1),
            UNIX_EPOCH + Duration::from_secs(3),
        )
        .unwrap();
        let profile = encode_reports(
            vec![Report::new(HashMap::from([
                (stack("read"), 3),
                (stack("write"), 7),
            ]))],
            100,
            range.clone(),
        );
        let profile = Profile::decode(profile.encode_to_vec().as_slice()).unwrap();
        let mut actual: Vec<_> = profile
            .sample
            .iter()
            .map(|sample| {
                let frames: Vec<_> = sample
                    .location_id
                    .iter()
                    .rev()
                    .flat_map(|id| {
                        let location = profile
                            .location
                            .iter()
                            .find(|location| location.id == *id)
                            .unwrap();
                        location.line.iter().map(|line| {
                            let function = profile
                                .function
                                .iter()
                                .find(|f| f.id == line.function_id)
                                .unwrap();
                            (
                                profile.string_table[function.name as usize].as_str(),
                                profile.string_table[function.filename as usize].as_str(),
                                line.line,
                            )
                        })
                    })
                    .collect();
                let mut labels: Vec<_> = sample
                    .label
                    .iter()
                    .map(|label| {
                        (
                            profile.string_table[label.key as usize].as_str(),
                            profile.string_table[label.str as usize].as_str(),
                        )
                    })
                    .collect();
                labels.sort();
                (frames, sample.value.clone(), labels)
            })
            .collect();
        actual.sort();
        let labels = vec![("pid", "42"), ("service", "fixture")];
        assert_eq!(
            actual,
            vec![
                (
                    vec![("main", "main.py", 10), ("read", "worker_é.py", 20)],
                    vec![30_000_000],
                    labels.clone()
                ),
                (
                    vec![("main", "main.py", 10), ("write", "worker_é.py", 20)],
                    vec![70_000_000],
                    labels
                ),
            ],
            "full profile: {profile:#?}"
        );
        assert_eq!(
            (profile.time_nanos, profile.duration_nanos, profile.period),
            (1_000_000_000, 2_000_000_000, 10_000_000),
            "{profile:#?}"
        );
        let empty = encode_reports(vec![], 100, range);
        assert!(empty.sample.is_empty(), "{empty:#?}");
        assert_eq!(
            (empty.time_nanos, empty.duration_nanos, empty.period),
            (profile.time_nanos, profile.duration_nanos, profile.period),
            "{empty:#?}"
        );
        assert_eq!(empty.sample_type.len(), 1, "{empty:#?}");
        assert_eq!(
            empty.string_table[empty.sample_type[0].r#type as usize], "cpu",
            "{empty:#?}"
        );
        assert_eq!(
            empty.string_table[empty.sample_type[0].unit as usize], "nanoseconds",
            "{empty:#?}"
        );
    }

    #[test]
    fn signal_report_consumes_drops_only_when_building_pprof() {
        use crate::{encode::r#gen::google::Profile, signal_backend::ProfileBuffer};
        use pyroscope_python_unwind::METRICS;
        use std::time::{Duration, UNIX_EPOCH};

        let _guard = crate::signal_sampler::METRICS_LOCK.lock().unwrap();
        METRICS.take_drop_counts();
        let buffer = Arc::new(Mutex::new(ProfileBuffer::new()));
        let reporter = Reporter::Signal(buffer);
        let range = TimeRange::new(UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(10)).unwrap();
        for count in [3, 2] {
            METRICS.resolve_error.fetch_add(count, Ordering::Relaxed);
            let report = reporter.report(100, &range).unwrap();
            let ReportData::RawPprof(bytes) = report.data else {
                panic!("signal report must already contain the pprof");
            };
            let profile = Profile::decode(bytes.as_slice()).unwrap();
            assert_eq!(profile.sample.len(), 1, "{profile:#?}");
            let sample = &profile.sample[0];
            let frames: Vec<_> = sample
                .location_id
                .iter()
                .rev()
                .flat_map(|id| {
                    let location = profile
                        .location
                        .iter()
                        .find(|location| location.id == *id)
                        .unwrap();
                    location.line.iter().map(|line| {
                        let function = profile
                            .function
                            .iter()
                            .find(|function| function.id == line.function_id)
                            .unwrap();
                        (
                            profile.string_table[function.name as usize].as_str(),
                            profile.string_table[function.filename as usize].as_str(),
                            line.line,
                        )
                    })
                })
                .collect();
            assert_eq!(
                frames,
                [("[dropped: resolve_error]", "", 0)],
                "{profile:#?}"
            );
            assert_eq!(sample.value, [count as i64 * 10_000_000], "{profile:#?}");
            assert!(sample.label.is_empty(), "{profile:#?}");
            assert!(reporter.report(100, &range).unwrap().is_empty());
        }
    }

    fn create_test_frame(
        name: &str,
        filename: &str,
        module: Option<&str>,
        line: i32,
    ) -> py_spy::Frame {
        py_spy::Frame {
            name: name.to_string(),
            filename: filename.to_string(),
            module: module.map(|s| s.to_string()),
            short_filename: None,
            line,
            locals: None,
            is_entry: false,
            is_shim_entry: false,
        }
    }

    #[test]
    fn test_frame_name_with_module() {
        // When module is provided (e.g., native frames or class methods),
        // name should be "module.function"
        let frame = create_test_frame(
            "find_longest_match",
            "/usr/lib/python3.12/difflib.py",
            Some("SequenceMatcher"),
            42,
        );

        let wrapper: StackFrameWrapper = frame.into();
        let stack_frame: StackFrame = wrapper.into();

        assert_eq!(
            stack_frame.name,
            "SequenceMatcher.find_longest_match".to_string()
        );
        // filename preserves the full absolute path
        assert_eq!(
            stack_frame.filename,
            "/usr/lib/python3.12/difflib.py".to_string()
        );
        assert_eq!(stack_frame.line, 42);
    }

    #[test]
    fn test_frame_name_without_module() {
        // When module is None, name should just be the function name
        let frame = create_test_frame("my_function", "/home/user/app/main.py", None, 10);

        let wrapper: StackFrameWrapper = frame.into();
        let stack_frame: StackFrame = wrapper.into();

        assert_eq!(stack_frame.name, "my_function".to_string());
        // filename preserves the full absolute path
        assert_eq!(stack_frame.filename, "/home/user/app/main.py".to_string());
        assert_eq!(stack_frame.line, 10);
    }

    #[test]
    fn test_frame_absolute_path_preserved() {
        // absolute_path should always contain the full path
        let frame = create_test_frame("test_func", "/path/to/file.py", None, 1);

        let wrapper: StackFrameWrapper = frame.into();
        let stack_frame: StackFrame = wrapper.into();

        assert_eq!(stack_frame.filename, "/path/to/file.py".to_string());
    }

    #[test]
    fn test_name_never_contains_path_separator() {
        // The function name field should NEVER contain path separators
        // This would indicate the filename is leaking into the name
        let test_cases = vec![
            ("func1", "/a/b/c.py", None),
            ("func2", "/very/long/path/to/module.py", Some("MyClass")),
            ("func3", "relative/path.py", None),
        ];

        for (func_name, filename, module) in test_cases {
            let frame = create_test_frame(func_name, filename, module, 1);
            let wrapper: StackFrameWrapper = frame.into();
            let stack_frame: StackFrame = wrapper.into();

            let name = stack_frame.name;
            assert!(
                !name.contains('/'),
                "Function name '{}' should not contain '/' path separator! Input: func={}, file={}, module={:?}",
                name,
                func_name,
                filename,
                module
            );
        }
    }
}
