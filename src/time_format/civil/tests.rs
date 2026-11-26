use super::*;
use proptest::prelude::*;

// Include the wide day range used by retention rendering, which works directly with i64 seconds.
const LAST_DAY: i64 = i64::MAX / 86_400;

proptest! {
    #[test]
    fn unix_days_round_trip_in_calendar_order(left in -LAST_DAY..=LAST_DAY, right in -LAST_DAY..=LAST_DAY) {
        let left_date = civil_from_days(left);
        let right_date = civil_from_days(right);
        prop_assert_eq!(days_from_civil(left_date), left);
        prop_assert_eq!(days_from_civil(right_date), right);
        prop_assert_eq!(left_date.cmp(&right_date), left.cmp(&right));
    }

    #[test]
    fn each_month_ends_exactly_one_day_before_the_next(year in -10_000i64..=10_000, month in 1i64..=12) {
        let last = days_in_month(year, month).unwrap();
        let date = CivilDate { year, month, day: last };
        let last_day = days_from_civil(date);
        let next = if month == 12 {
            CivilDate { year: year + 1, month: 1, day: 1 }
        } else {
            CivilDate { year, month: month + 1, day: 1 }
        };
        prop_assert_eq!(civil_from_days(last_day), date);
        prop_assert_eq!(civil_from_days(last_day + 1), next);
        prop_assert_eq!(days_from_civil(next), last_day + 1);
    }
}

#[test]
fn calendar_pins_the_epoch_and_gregorian_century_boundaries() {
    let date = |year, month, day| CivilDate { year, month, day };
    assert_eq!(civil_from_days(-1), date(1969, 12, 31));
    assert_eq!(civil_from_days(0), date(1970, 1, 1));
    assert_eq!(days_from_civil(date(2000, 2, 29)), 11_016);
    for (year, february) in [(0, 29), (1900, 28), (2000, 29), (2100, 28)] {
        assert_eq!(days_in_month(year, 2), Some(february));
        assert_eq!(
            civil_from_days(days_from_civil(date(year, 3, 1)) - 1),
            date(year, 2, february)
        );
    }
    assert_eq!(days_in_month(2000, 0), None);
    assert_eq!(days_in_month(2000, 13), None);
}

#[test]
fn civil_conversions_agree_with_counting_each_day_in_the_wire_calendar() {
    use crate::clock::{RFC3339_MAX_MICROS, RFC3339_MIN_MICROS};
    const MICROS_PER_DAY: i64 = 86_400_000_000;
    let mut unix_day = RFC3339_MIN_MICROS.div_euclid(MICROS_PER_DAY);
    let mut expected_weekday = 6; // 0000-01-01 was a Saturday.
    let ordinary_months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for year in 0..=9999 {
        let leap = year % 4 == 0 && !matches!(year % 400, 100 | 200 | 300);
        for (month, ordinary_days) in (1..=12).zip(ordinary_months) {
            let count = ordinary_days + i64::from(leap && month == 2);
            assert_eq!(days_in_month(year, month), Some(count));
            for day in 1..=count {
                let date = CivilDate { year, month, day };
                assert_eq!(civil_from_days(unix_day), date);
                assert_eq!(days_from_civil(date), unix_day);
                assert_eq!(weekday(unix_day), expected_weekday);
                unix_day += 1;
                expected_weekday = (expected_weekday + 1) % 7;
            }
        }
    }
    assert_eq!(unix_day - 1, RFC3339_MAX_MICROS.div_euclid(MICROS_PER_DAY));
}
