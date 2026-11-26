use std::{future::Future, pin::Pin, time::SystemTime};

pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;

    /// Blocks the calling thread for `duration` as this clock tells time, for work on a
    /// blocking pool that waits before it tries again. A test clock advances instead.
    fn pause(&self, duration: std::time::Duration) {
        std::thread::sleep(duration);
    }

    /// Waits for `duration` as this clock tells time, for a background task that works on a
    /// schedule. A test clock ends the wait once a test has moved it past the wait's end.
    fn sleep(
        &self,
        duration: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(tokio::time::sleep(duration))
    }

    /// Waits for a stored deadline on this clock, including when it advances before the first poll.
    fn sleep_until(&self, deadline: SystemTime) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.sleep(deadline.duration_since(self.now()).unwrap_or_default())
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// `time` as signed nanoseconds since the Unix epoch.
pub(crate) fn signed_nanos(time: SystemTime) -> i128 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
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

/// A clock that moves only when a test moves it. A watch channel holds the time so that a wait
/// on the clock ([`Clock::sleep`]) ends when a move passes its end.
#[cfg(test)]
pub(crate) struct ManualClock {
    time: tokio::sync::watch::Sender<SystemTime>,
    reads: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl ManualClock {
    pub(crate) fn new(now: SystemTime) -> Self {
        Self {
            time: tokio::sync::watch::Sender::new(now),
            reads: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(crate) fn set(&self, now: SystemTime) {
        self.time.send_replace(now);
    }

    /// Leaves the scheduled waiter asleep so the test can attribute its wake to a request.
    pub(crate) fn set_without_waking(&self, now: SystemTime) {
        self.time.send_if_modified(|previous| {
            *previous = now;
            false
        });
    }

    /// Explicit time reads and observations made when arming waits.
    pub(crate) fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn advance(&self, duration: std::time::Duration) {
        self.time.send_modify(|now| *now += duration);
    }
}

#[cfg(test)]
impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.time.borrow()
    }

    fn pause(&self, duration: std::time::Duration) {
        self.advance(duration);
    }

    fn sleep(
        &self,
        duration: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.sleep_until(self.now() + duration)
    }

    fn sleep_until(&self, end: SystemTime) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut time = self.time.subscribe();
        Box::pin(async move {
            // The sender lives as long as the clock this wait borrows, so the wait ends only when
            // the time reaches its end.
            time.wait_for(|now| *now >= end).await.ok();
        })
    }
}

/// 0000-01-01T00:00:00Z: the shared wire range includes year 0000 for existing native catalogs.
pub(crate) const RFC3339_MIN_MICROS: i64 = -62_167_219_200_000_000;
/// 9999-12-31T23:59:59.999999Z.
pub(crate) const RFC3339_MAX_MICROS: i64 = 253_402_300_799_999_999;
pub(crate) const MICROS_PER_SECOND: i64 = 1_000_000;
/// 0001-01-01T00:00:00Z: Go's zero `time.Time`, which a fake-gcs-server export writes for an
/// unset time, and the earliest protobuf `Timestamp`.
pub(crate) const YEAR_ONE_MICROS: i64 = -62_135_596_800_000_000;

/// The shared wire range includes year 0000 for existing native catalog compatibility.
pub(crate) fn rfc3339_micros(micros: i128) -> bool {
    (i128::from(RFC3339_MIN_MICROS)..=i128::from(RFC3339_MAX_MICROS)).contains(&micros)
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
