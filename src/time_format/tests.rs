use super::*;
use crate::{
    clock::{RFC3339_MAX_MICROS, RFC3339_MIN_MICROS, system_time_from_unix_seconds},
    import::codec::parse_timestamp,
};
use proptest::prelude::*;
use std::time::Duration;

#[test]
fn http_header_dates_omit_unrenderable_times() {
    use std::time::UNIX_EPOCH;
    assert_eq!(
        http_date_value(UNIX_EPOCH).unwrap(),
        "Thu, 01 Jan 1970 00:00:00 GMT"
    );
    assert!(http_date_value(UNIX_EPOCH - Duration::from_secs(1)).is_none());
    let after_year_9999 = system_time_from_unix_seconds(253_402_300_800).unwrap();
    assert!(http_date_value(after_year_9999).is_none());
}

proptest! {
    #[test]
    fn timestamp_text_round_trips_at_wire_precision(
        micros in RFC3339_MIN_MICROS..=RFC3339_MAX_MICROS,
        nanos in 0u32..1000,
    ) {
        let time = UnixNanos::from_micros_and_remainder(micros, nanos).unwrap();
        prop_assert_eq!(parse_timestamp(&timestamp_nanos(time).unwrap()), Ok(micros.div_euclid(1000) * 1000));
        prop_assert_eq!(parse_timestamp(&rfc3339(time, Fraction::Micros).unwrap()), Ok(micros));
    }

    #[test]
    fn timestamp_text_sorts_by_its_displayed_instant(
        left in RFC3339_MIN_MICROS..=RFC3339_MAX_MICROS,
        offset in -2000i64..=2000,
        distant in RFC3339_MIN_MICROS..=RFC3339_MAX_MICROS,
    ) {
        let a = UnixNanos::from_micros_and_remainder(left, 0).unwrap();
        let nearby = (left + offset).clamp(RFC3339_MIN_MICROS, RFC3339_MAX_MICROS);
        for right in [nearby, distant] {
            let b = UnixNanos::from_micros_and_remainder(right, 0).unwrap();
            prop_assert_eq!(timestamp_nanos(a).unwrap().cmp(&timestamp_nanos(b).unwrap()),
                left.div_euclid(1000).cmp(&right.div_euclid(1000)));
            prop_assert_eq!(rfc3339(a, Fraction::Micros).unwrap().cmp(&rfc3339(b, Fraction::Micros).unwrap()),
                left.cmp(&right));
        }
    }

    #[test]
    fn http_date_text_round_trips_to_whole_seconds(
        seconds in 0i64..=RFC3339_MAX_MICROS / 1_000_000,
        millis in 0u64..1000,
    ) {
        let whole = system_time_from_unix_seconds(seconds).unwrap();
        let formatted = http_date(whole + Duration::from_millis(millis)).unwrap();
        prop_assert_eq!(httpdate::parse_http_date(&formatted).unwrap(), whole);
    }
}
