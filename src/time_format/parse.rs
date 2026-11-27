//! RFC 3339 text parsed into microseconds since the Unix epoch, the inverse of the renderings in
//! `time_format`. Import snapshots, fixtures, custom times and POST policy expirations share it.

use super::civil::{CivilDate, days_from_civil, days_in_month};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub(crate) struct TimestampError(&'static str);

impl TimestampError {
    /// A failure that a caller reports in its own words about an instant this parser accepted,
    /// such as one its clock cannot hold.
    pub(crate) const fn new(message: &'static str) -> Self {
        Self(message)
    }
}

impl From<TimestampError> for String {
    fn from(error: TimestampError) -> Self {
        error.0.into()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Precision {
    Micros,
    Nanos,
}

pub(crate) fn parse_timestamp(input: &str) -> Result<i64, TimestampError> {
    parse_timestamp_parts(input, Precision::Micros).map(|(micros, _)| micros)
}

/// An instant with up to nanosecond digits, floored to the microsecond. Only for a deadline:
/// dropping the sub-microsecond digits can end it earlier, never later.
pub(crate) fn parse_deadline_timestamp(input: &str) -> Result<i64, TimestampError> {
    parse_timestamp_parts(input, Precision::Nanos).map(|(micros, _)| micros)
}

/// An instant with up to nanosecond digits, as microseconds and the nanoseconds below them.
pub(crate) fn parse_nanosecond_timestamp(input: &str) -> Result<(i64, u32), TimestampError> {
    parse_timestamp_parts(input, Precision::Nanos)
}

fn parse_timestamp_parts(input: &str, precision: Precision) -> Result<(i64, u32), TimestampError> {
    let invalid =
        TimestampError("invalid source timestamp; supply a complete RFC3339 source timestamp");
    let bytes = input.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return Err(invalid);
    }
    let number = |start, end| -> Result<i64, TimestampError> {
        bytes
            .get(start..end)
            .ok_or(invalid)?
            .iter()
            .try_fold(0i64, |n, b| {
                if b.is_ascii_digit() {
                    Ok(n * 10 + i64::from(b - b'0'))
                } else {
                    Err(invalid)
                }
            })
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let days = days_in_month(year, month).ok_or(invalid)?;
    if day < 1 || day > days || hour > 23 || minute > 59 || second > 59 {
        return Err(invalid);
    }
    let mut index = 19;
    let mut micros = 0;
    let mut nanos = 0;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while let Some(&byte) = bytes.get(index).filter(|byte| byte.is_ascii_digit()) {
            let digit = byte - b'0';
            if index - start < 6 {
                micros = micros * 10 + i64::from(digit);
            } else if index - start < 9 && precision == Precision::Nanos {
                nanos = nanos * 10 + u32::from(digit);
            } else if digit != 0 {
                if precision == Precision::Nanos {
                    return Err(TimestampError(
                        "nonzero sub-nanosecond timestamp digits; retain the exact source timestamp without truncation",
                    ));
                }
                return Err(TimestampError(
                    "nonzero sub-microsecond timestamp digits; use an exact-microsecond snapshot, without truncation",
                ));
            }
            index += 1;
        }
        if index == start {
            return Err(invalid);
        }
        for _ in (index - start)..6 {
            micros *= 10;
        }
        for _ in (index - start).max(6)..9 {
            nanos *= 10;
        }
    }
    let zone = bytes.get(index..).ok_or(invalid)?;
    let offset = if zone == b"Z" {
        0
    } else if let [sign @ (b'+' | b'-'), _, _, b':', _, _] = zone {
        let zh = number(index + 1, index + 3)?;
        let zm = number(index + 4, index + 6)?;
        if zh > 23 || zm > 59 {
            return Err(invalid);
        }
        (zh * 3600 + zm * 60) * if *sign == b'-' { -1 } else { 1 }
    } else {
        return Err(invalid);
    };
    let days = days_from_civil(CivilDate { year, month, day });
    let micros = (days * 86400 + hour * 3600 + minute * 60 + second - offset) * 1_000_000 + micros;
    if !crate::clock::rfc3339_micros(i128::from(micros)) {
        return Err(TimestampError(
            "timestamp is outside the supported RFC3339 UTC range after timezone normalization",
        ));
    }
    Ok((micros, nanos))
}
