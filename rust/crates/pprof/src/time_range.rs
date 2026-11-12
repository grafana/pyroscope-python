use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, PartialEq, Clone)]
pub struct TimeRange {
    from_unix: Duration,
    duration: Duration,
}

impl TimeRange {
    pub fn new(
        from: SystemTime,
        until: SystemTime,
    ) -> Result<TimeRange, std::time::SystemTimeError> {
        let from_unix = from.duration_since(UNIX_EPOCH)?;
        let duration = until.duration_since(from).unwrap_or(Duration::ZERO);
        Ok(Self {
            from_unix,
            duration,
        })
    }

    pub fn start_time_unix(&self) -> Duration {
        self.from_unix
    }
    pub fn duration(&self) -> Duration {
        self.duration
    }
}
