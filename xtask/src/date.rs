//! UTC calendar dates as the ledgers record them, `YYYY-MM-DD`.

use std::{fmt, str::FromStr};

/// A Gregorian calendar date. A parsed date lies between 0001-01-01 and 9999-12-31; the fields are
/// declared from year to day, so the derived order is chronological.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Date {
    year: u64,
    month: u8,
    day: u8,
}

impl Date {
    /// The UTC date of an instant `seconds` after the Unix epoch.
    pub(crate) fn from_unix_seconds(seconds: u64) -> Self {
        // Howard Hinnant's civil_from_days, shifted so that every intermediate value is
        // non-negative: day 0 is 0000-03-01, and each 400-year era has 146,097 days.
        let days = seconds / 86_400 + 719_468;
        let era = days / 146_097;
        let day_of_era = days % 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        // Months counted from March, so the leap day ends the year.
        let march_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * march_month + 2) / 5 + 1;
        let month = if march_month < 10 {
            march_month + 3
        } else {
            march_month - 9
        };
        let year = era * 400 + year_of_era + u64::from(month <= 2);
        // `month` is 1 to 12 and `day` 1 to 31, so neither cast drops a bit.
        Self {
            year,
            month: month as u8,
            day: day as u8,
        }
    }
}

/// A text that is not a `YYYY-MM-DD` calendar date; it names the rejected text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InvalidDate(String);

impl fmt::Display for InvalidDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} is not a calendar date in YYYY-MM-DD form", self.0)
    }
}

impl std::error::Error for InvalidDate {}

/// Accepts exactly four year digits, two month digits and two day digits, and only a day that the
/// month has in that year.
impl FromStr for Date {
    type Err = InvalidDate;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidDate(text.to_owned());
        let mut parts = text.split('-');
        let (Some(year), Some(month), Some(day), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid());
        };
        let (Some(year), Some(month), Some(day)) =
            (digits(year, 4), digits(month, 2), digits(day, 2))
        else {
            return Err(invalid());
        };
        if year == 0 || !(1..=days_in_month(year, month)).contains(&day) {
            return Err(invalid());
        }
        Ok(Self { year, month, day })
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// A field of exactly `width` ASCII digits.
fn digits<T: FromStr>(field: &str, width: usize) -> Option<T> {
    if field.len() != width || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

/// The days of `month` in `year`, or 0 for a month that does not exist.
fn days_in_month(year: u64, month: u8) -> u8 {
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_give_the_utc_date_of_that_instant() {
        for (seconds, date) in [
            (0, "1970-01-01"),
            (951_868_799, "2000-02-29"),
            (1_795_737_599, "2026-11-26"),
            (1_795_737_600, "2026-11-27"),
            (4_107_542_399, "2100-02-28"),
            (4_107_542_400, "2100-03-01"),
        ] {
            assert_eq!(Date::from_unix_seconds(seconds).to_string(), date);
            assert_eq!(date.parse(), Ok(Date::from_unix_seconds(seconds)));
        }
    }

    #[test]
    fn only_real_calendar_dates_in_the_ledger_form_parse() {
        for valid in [
            "0001-01-01",
            "2024-02-29",
            "2000-02-29",
            "2026-11-26",
            "9999-12-31",
        ] {
            assert_eq!(valid.parse::<Date>().unwrap().to_string(), valid);
        }
        for invalid in [
            "",
            "0000-01-01",
            "2026-02-29",
            "2100-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-04-31",
            "2026-11-00",
            "2026-1-26",
            "26-11-26",
            "2026-11-26 ",
            "2026/11/26",
            "2026-11-26-01",
            "+026-11-26",
            "2026-11-2a",
        ] {
            assert_eq!(
                invalid.parse::<Date>(),
                Err(InvalidDate(invalid.to_owned())),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn dates_order_chronologically() {
        let parse = |text: &str| text.parse::<Date>().unwrap();
        assert!(parse("2026-11-26") < parse("2026-11-27"));
        assert!(parse("2026-11-30") < parse("2026-12-01"));
        assert!(parse("2026-12-31") < parse("2027-01-01"));
    }
}
