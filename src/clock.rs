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

/// The shared wire range includes year 0000 for existing native catalog compatibility.
pub(crate) fn rfc3339_micros(micros: i128) -> bool {
    (-62_167_219_200_000_000..=253_402_300_799_999_999).contains(&micros)
}
