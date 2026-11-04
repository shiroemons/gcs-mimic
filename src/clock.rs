use std::time::SystemTime;

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

pub(crate) fn system_time_from_unix_seconds(seconds: i64) -> Option<SystemTime> {
    let duration = std::time::Duration::from_secs(seconds.unsigned_abs());
    if seconds < 0 {
        SystemTime::UNIX_EPOCH.checked_sub(duration)
    } else {
        SystemTime::UNIX_EPOCH.checked_add(duration)
    }
}

#[cfg(test)]
pub(crate) struct ManualClock(std::sync::Mutex<SystemTime>);

#[cfg(test)]
impl ManualClock {
    pub(crate) fn new(now: SystemTime) -> Self {
        Self(std::sync::Mutex::new(now))
    }

    #[track_caller]
    pub(crate) fn set(&self, now: SystemTime) {
        *self.0.lock().unwrap() = now;
    }

    #[track_caller]
    pub(crate) fn advance(&self, duration: std::time::Duration) {
        *self.0.lock().unwrap() += duration;
    }
}

#[cfg(test)]
impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}

/// The shared wire range includes year 0000 for existing native catalog compatibility.
pub(crate) fn rfc3339_micros(micros: i128) -> bool {
    (-62_167_219_200_000_000..=253_402_300_799_999_999).contains(&micros)
}

/// A clock whose instants are whole microseconds, as every catalog written before schema 46
/// stored them. Tests that downgrade a catalog and migrate it again need this precision: the
/// migration rebuilds a bucket generation from the stored microsecond creation time, so a
/// nanosecond clock (Linux) yields a generation the rebuilt record cannot reproduce.
#[cfg(test)]
pub(crate) struct MicrosecondClock;

#[cfg(test)]
impl Clock for MicrosecondClock {
    fn now(&self) -> SystemTime {
        let now = SystemTime::now();
        let nanos = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |elapsed| u64::from(elapsed.subsec_nanos()) % 1_000);
        now - std::time::Duration::from_nanos(nanos)
    }
}
