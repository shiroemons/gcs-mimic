use super::*;
use proptest::prelude::*;

// Include the wide day range used by retention rendering, which works directly with i64 seconds.
const LAST_DAY: i64 = i64::MAX / 86_400;

proptest! {
    #[test]
    fn unix_days_round_trip_in_calendar_order(left in -LAST_DAY..=LAST_DAY, right in -LAST_DAY..=LAST_DAY) {
        let left_date = civil_from_days(left);
        let right_date = civil_from_days(right);
        prop_assert_eq!(days_from_civil(left_date.0, left_date.1, left_date.2), left);
        prop_assert_eq!(days_from_civil(right_date.0, right_date.1, right_date.2), right);
        prop_assert_eq!(left_date.cmp(&right_date), left.cmp(&right));
    }

    #[test]
    fn each_month_ends_exactly_one_day_before_the_next(year in -10_000i64..=10_000, month in 1i64..=12) {
        let last = days_in_month(year, month).unwrap();
        let last_day = days_from_civil(year, month, last);
        let next = if month == 12 { (year + 1, 1, 1) } else { (year, month + 1, 1) };
        prop_assert_eq!(civil_from_days(last_day), (year, month, last));
        prop_assert_eq!(civil_from_days(last_day + 1), next);
        prop_assert_eq!(days_from_civil(next.0, next.1, next.2), last_day + 1);
    }
}

#[test]
fn calendar_pins_the_epoch_and_gregorian_century_boundaries() {
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(days_from_civil(2000, 2, 29), 11_016);
    for (year, february) in [(0, 29), (1900, 28), (2000, 29), (2100, 28)] {
        assert_eq!(days_in_month(year, 2), Some(february));
        assert_eq!(
            civil_from_days(days_from_civil(year, 3, 1) - 1),
            (year, 2, february)
        );
    }
    assert_eq!(days_in_month(2000, 0), None);
    assert_eq!(days_in_month(2000, 13), None);
}
