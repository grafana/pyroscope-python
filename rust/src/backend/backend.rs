#![allow(clippy::module_inception)]

use std::fmt::Debug;

#[derive(Debug, Copy, Clone, Default)]
pub struct BackendConfig {
    pub report_thread_id: bool,
    pub report_thread_name: bool,
    pub report_pid: bool,
}
