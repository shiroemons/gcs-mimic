//! Proleptic Gregorian calendar arithmetic independent of the platform clock.

// A 400-year cycle starts in March so February's leap day is last in its year.
const DAYS_PER_ERA: i64 = 146_097;
const UNIX_EPOCH_FROM_MARCH: i64 = 719_468;

/// Days since 1970-01-01. Callers decide which calendar dates their grammar admits.
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * DAYS_PER_ERA + day_of_era - UNIX_EPOCH_FROM_MARCH
}

/// The year, month and day of a Unix day count derived from a timestamp.
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + UNIX_EPOCH_FROM_MARCH;
    let era = shifted.div_euclid(DAYS_PER_ERA);
    let day_of_era = shifted.rem_euclid(DAYS_PER_ERA);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = month_from_march + if month_from_march < 10 { 3 } else { -9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

pub(crate) fn days_in_month(year: i64, month: i64) -> Option<i64> {
    Some(match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
