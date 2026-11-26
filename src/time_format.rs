//! Stored times rendered as text: RFC 3339 timestamps in resources and listings, and RFC 9110
//! dates in headers. The JSON, XML and gRPC adapters and signing share these renderings; each
//! answers a time it cannot render in its own protocol.

use crate::object::unix_nanos::UnixNanos;
use std::time::SystemTime;

pub(crate) mod civil;

#[cfg(test)]
mod tests;

/// A stored time outside the RFC 3339 range that every rendering shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}", Self::MESSAGE)]
pub(crate) struct TimeOutOfRange;

impl TimeOutOfRange {
    pub(crate) const MESSAGE: &str = "Bucket timestamp is outside the supported RFC 3339 range";
}

/// Weekday names from 1970-01-01, a Thursday.
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A UTC instant broken into the calendar fields that both RFC 3339 and HTTP dates render.
struct CivilTime {
    year: i64,
    /// 1 to 12.
    month: i64,
    day: i64,
    /// Seconds into the day.
    daytime: i64,
    /// Microseconds into the second.
    micros: i64,
    /// Days since 1970-01-01, which fixes the weekday.
    days: i64,
}

/// The calendar fields of `time`, computed without a platform calendar. Fails outside the
/// RFC 3339 range that every rendered timestamp shares.
fn civil_time(time: UnixNanos) -> Result<CivilTime, TimeOutOfRange> {
    let micros = i128::from(time.secs()) * 1_000_000 + i128::from(time.subsec_nanos() / 1000);
    if !crate::clock::rfc3339_micros(micros) {
        return Err(TimeOutOfRange);
    }
    let micros = i64::try_from(micros).map_err(|_| TimeOutOfRange)?;
    let seconds = micros.div_euclid(1_000_000);
    let days = seconds.div_euclid(86400);
    let (year, month, day) = civil::civil_from_days(days);
    Ok(CivilTime {
        year,
        month,
        day,
        daytime: seconds.rem_euclid(86400),
        micros: micros.rem_euclid(1_000_000),
        days,
    })
}

/// The RFC 9110 date of a stored timestamp, as `Last-Modified` carries it.
pub(crate) fn http_date(time: SystemTime) -> Result<String, TimeOutOfRange> {
    let CivilTime {
        year,
        month,
        day,
        daytime,
        days,
        ..
    } = civil_time(UnixNanos::from(time))?;
    // A negative day count is an instant before the epoch, which is never rendered as a date.
    let days = usize::try_from(days).map_err(|_| TimeOutOfRange)?;
    let weekday = WEEKDAYS.get(days % 7).ok_or(TimeOutOfRange)?;
    let month = usize::try_from(month - 1)
        .ok()
        .and_then(|index| MONTHS.get(index))
        .ok_or(TimeOutOfRange)?;
    Ok(format!(
        "{weekday}, {day:02} {month} {year:04} {:02}:{:02}:{:02} GMT",
        daytime / 3600,
        daytime / 60 % 60,
        daytime % 60,
    ))
}

/// Render a stored time as RFC 3339 UTC with exactly three fractional digits, the precision
/// Cloud Storage emits (observed 2026-10-31). Stored times keep their finer precision; only
/// the rendering floors, so ordering and stored deadlines are unaffected.
pub(crate) fn timestamp(time: SystemTime) -> Result<String, TimeOutOfRange> {
    timestamp_nanos(UnixNanos::from(time))
}

/// [`timestamp`] for a time that is already seconds and nanoseconds, such as a retention time.
pub(crate) fn timestamp_nanos(time: UnixNanos) -> Result<String, TimeOutOfRange> {
    rfc3339(time, Fraction::Millis)
}

/// Render a stored time as RFC 3339 UTC with exactly six fractional digits, as Cloud Storage
/// writes XML multipart `Initiated` and part `LastModified` times (recorded 2026-11-16).
pub(crate) fn timestamp_micros(time: SystemTime) -> Result<String, TimeOutOfRange> {
    rfc3339(UnixNanos::from(time), Fraction::Micros)
}

/// The fractional-second digits of a rendered time; finer precision is floored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fraction {
    Millis,
    Micros,
}

fn rfc3339(time: UnixNanos, fraction: Fraction) -> Result<String, TimeOutOfRange> {
    let CivilTime {
        year,
        month,
        day,
        daytime,
        micros,
        ..
    } = civil_time(time)?;
    let fraction = match fraction {
        Fraction::Millis => format!("{:03}", micros / 1000),
        Fraction::Micros => format!("{micros:06}"),
    };
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{fraction}Z",
        daytime / 3600,
        daytime / 60 % 60,
        daytime % 60,
    ))
}
