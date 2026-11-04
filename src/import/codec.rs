#![deny(clippy::unwrap_used, clippy::expect_used)]

use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, Visitor},
};
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimestampError(&'static str);

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for TimestampError {}

impl From<TimestampError> for String {
    fn from(error: TimestampError) -> Self {
        error.0.into()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "PascalCase")]
pub(crate) struct SourceAcl {
    entity: String,
    role: String,
    #[serde(rename = "EntityID")]
    entity_id: String,
    domain: String,
    email: String,
    project_team: Option<serde_json::Value>,
}

pub(super) fn acl(
    values: Option<&[SourceAcl]>,
    kind: crate::acl::AclKind,
) -> Result<Option<Vec<crate::acl::AclEntry>>, std::io::Error> {
    let Some(values) = values else {
        return Ok(None);
    };
    let mut entries = Vec::with_capacity(values.len());
    for source in values {
        if !source.entity_id.is_empty()
            || !source.domain.is_empty()
            || !source.email.is_empty()
            || source.project_team.is_some()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ACL auxiliary identity fields cannot be preserved; retain the source snapshot",
            ));
        }
        entries.push(crate::acl::AclEntryInput {
            entity: source.entity.clone(),
            role: source.role.clone(),
        });
    }
    crate::acl::parse_entries(kind, entries)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

pub(super) fn retention(
    attrs: &ObjectAttrs,
) -> Result<Option<crate::object::retention::ObjectRetention>, std::io::Error> {
    let invalid = |reason| std::io::Error::new(std::io::ErrorKind::InvalidData, reason);
    let unset = is_unset_time(&attrs.retention_retain_until)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if attrs.retention_mode.is_empty() && unset {
        return Ok(None);
    }
    let mode = crate::object::retention::RetentionMode::parse(&attrs.retention_mode)
        .ok_or_else(|| invalid("source retention mode must be Locked or Unlocked"))?;
    if unset {
        return Err(invalid(
            "source retention mode requires a nonzero retain-until time",
        ));
    }
    // Nanosecond precision, like the JSON adapter: retention times compare exactly.
    let retain_until = parse_retention_timestamp(&attrs.retention_retain_until)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(Some(crate::object::retention::ObjectRetention {
        mode,
        retain_until,
    }))
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "PascalCase")]
pub(crate) struct BucketAttrs {
    pub default_event_based_hold: bool,
    #[serde(
        deserialize_with = "present_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub versioning_enabled: Option<bool>,
    #[serde(rename = "ACL")]
    pub acl: Option<Vec<SourceAcl>>,
}

fn present_bool<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    bool::deserialize(deserializer).map(Some)
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "PascalCase")]
pub(crate) struct ObjectAttrs {
    pub storage_class: String,
    pub content_type: String,
    pub content_encoding: String,
    pub content_disposition: String,
    pub content_language: String,
    pub cache_control: String,
    pub crc32c: String,
    pub md5_hash: String,
    pub etag: String,
    #[serde(rename = "ACL")]
    pub acl: Option<Vec<SourceAcl>>,
    #[serde(deserialize_with = "unique_metadata")]
    pub metadata: Option<BTreeMap<String, String>>,
    pub created: String,
    pub deleted: String,
    pub updated: String,
    pub custom_time: String,
    pub generation: i64,
    pub retention_mode: String,
    pub retention_retain_until: String,
}

fn unique_metadata<'de, D: de::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, String>>, D::Error> {
    struct Map;
    impl<'de> Visitor<'de> for Map {
        type Value = Option<BTreeMap<String, String>>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a string map or null")
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_map(self)
        }
        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if values.insert(key, value).is_some() {
                    return Err(de::Error::custom("duplicate metadata key"));
                }
            }
            Ok(Some(values))
        }
    }
    deserializer.deserialize_option(Map)
}

#[cfg(test)]
pub(crate) fn path_escape(name: &str) -> String {
    let mut result = String::new();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~$&+:=@".contains(&byte) {
            result.push(char::from(byte));
        } else {
            result.push('%');
            result.push(char::from(HEX[(byte >> 4) as usize]));
            result.push(char::from(HEX[(byte & 15) as usize]));
        }
    }
    result
}

pub(crate) fn path_unescape(name: &str) -> Result<String, std::io::Error> {
    let mut result = Vec::with_capacity(name.len());
    let mut bytes = name.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            match bytes
                .next()
                .zip(bytes.next())
                .and_then(|(high, low)| crate::hex::byte(&[high, low]))
            {
                Some(byte) => result.push(byte),
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "malformed bucket percent escape; restore the escaped directory name",
                    ));
                }
            }
        } else {
            result.push(byte);
        }
    }
    String::from_utf8(result).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            crate::storage::failure::BackendCause::with_source(
                "decoded bucket must be UTF-8; restore the original snapshot",
                error,
            ),
        )
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Precision {
    Micros,
    Nanos,
}

pub(crate) fn parse_timestamp(input: &str) -> Result<i64, TimestampError> {
    parse_timestamp_parts(input, Precision::Micros).map(|(micros, _)| micros)
}

/// A retention time to the nanosecond, as seconds and nanoseconds. It never passes through a
/// `SystemTime`, so a platform that floors to 100 ns still keeps every digit.
pub(super) fn parse_retention_timestamp(
    input: &str,
) -> Result<crate::object::unix_nanos::UnixNanos, TimestampError> {
    let (micros, nanos) = parse_timestamp_parts(input, Precision::Nanos)?;
    crate::object::unix_nanos::UnixNanos::from_micros_and_remainder(micros, nanos).ok_or(
        TimestampError("retention timestamp is outside the supported range"),
    )
}

pub(super) fn parse_bucket_timestamp(input: &str) -> Result<std::time::SystemTime, std::io::Error> {
    let invalid =
        |reason| std::io::Error::new(std::io::ErrorKind::InvalidData, TimestampError(reason));
    let (micros, nanos) = parse_timestamp_parts(input, Precision::Nanos)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let base = super::filesystem::time(micros).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            crate::storage::failure::BackendCause::with_source(
                "bucket timestamp is outside this platform's range",
                error,
            ),
        )
    })?;
    let remainder = std::time::Duration::from_nanos(u64::from(nanos));
    let time = base
        .checked_add(remainder)
        .ok_or_else(|| invalid("bucket timestamp is outside this platform's range"))?;
    // Windows keeps 100 ns intervals, so sub-interval digits would be dropped silently;
    // reject them like other unrepresentable source precision.
    if time.duration_since(base).ok() != Some(remainder) {
        return Err(invalid(
            "bucket timestamp is finer than this platform's clock resolution",
        ));
    }
    Ok(time)
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
    let (mut year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return Err(invalid),
    };
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
    // Gregorian civil-date conversion keeps pre-epoch instants exact without a time dependency.
    year -= i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let micros = ((era * 146097 + doe - 719468) * 86400 + hour * 3600 + minute * 60 + second
        - offset)
        * 1_000_000
        + micros;
    if !crate::clock::rfc3339_micros(i128::from(micros)) {
        return Err(TimestampError(
            "timestamp is outside the supported RFC3339 UTC range after timezone normalization",
        ));
    }
    Ok((micros, nanos))
}

pub(crate) fn is_unset_time(input: &str) -> Result<bool, TimestampError> {
    Ok(input.is_empty() || parse_timestamp(input)? == -62_135_596_800_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn source_acl_validation_keeps_count_and_entity_ahead_of_role_errors() {
        for (count, entity, expected) in [
            (101, "allUsers", crate::acl::AclError::TooManyEntries),
            (1, "bad", crate::acl::AclError::InvalidEntity("bad".into())),
        ] {
            let entries = (0..count)
                .map(|_| SourceAcl {
                    entity: entity.into(),
                    role: "invalid".into(),
                    ..Default::default()
                })
                .collect::<Vec<_>>();
            let error = acl(Some(&entries), crate::acl::AclKind::Object).unwrap_err();
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::InvalidData,
                "{count} {entity}"
            );
            assert_eq!(
                error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<crate::acl::AclError>(),
                Some(&expected),
                "{count} {entity}"
            );
        }
    }

    #[test]
    fn snapshot_acl_and_storage_class_keep_source_spelling() {
        for class in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            let input = format!(
                r#"{{"StorageClass":"{class}","ACL":[{{"Entity":"allUsers","Role":"READER"}}]}}"#
            );
            let attrs: ObjectAttrs = serde_json::from_str(&input).unwrap();
            let encoded = serde_json::to_value(&attrs).unwrap();
            assert_eq!(encoded["StorageClass"], class, "{class}");
            assert_eq!(encoded["ACL"][0]["Role"], "READER", "{class}");
            let entries = acl(attrs.acl.as_deref(), crate::acl::AclKind::Object)
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_string(&entries).unwrap(),
                r#"[{"entity":"allUsers","role":"READER"}]"#,
                "{class}"
            );
        }
    }
    #[test]
    fn bucket_versioning_distinguishes_absent_and_false_without_accepting_null() {
        for (input, expected) in [
            (r#"{}"#, None),
            (r#"{"VersioningEnabled":false}"#, Some(false)),
            (r#"{"VersioningEnabled":true}"#, Some(true)),
        ] {
            let attrs: BucketAttrs = serde_json::from_str(input).unwrap();
            assert_eq!(attrs.versioning_enabled, expected);
        }
        for invalid in [
            r#"{"VersioningEnabled":null}"#,
            r#"{"VersioningEnabled":"false"}"#,
        ] {
            assert!(serde_json::from_str::<BucketAttrs>(invalid).is_err());
        }
    }
    #[test]
    fn source_acl_duplicate_identity_fields_are_not_silently_overwritten() {
        assert!(serde_json::from_slice::<ObjectAttrs>(
            br#"{"ACL":[{"Entity":"allUsers","Entity":"allAuthenticatedUsers","Role":"READER"}]}"#
        ).is_err());
    }
    #[test]
    fn bucket_nanoseconds_preserve_negative_instants_without_relaxing_object_precision() {
        use std::time::{Duration, SystemTime, UNIX_EPOCH};
        // The exact instant when this platform's SystemTime can hold it (Windows keeps 100 ns).
        fn exact(nanos: i64) -> Option<SystemTime> {
            let offset = Duration::from_nanos(nanos.unsigned_abs());
            let time = if nanos < 0 {
                UNIX_EPOCH.checked_sub(offset)?
            } else {
                UNIX_EPOCH.checked_add(offset)?
            };
            let held = if nanos < 0 {
                UNIX_EPOCH.duration_since(time)
            } else {
                time.duration_since(UNIX_EPOCH)
            };
            (held.ok() == Some(offset)).then_some(time)
        }
        for (input, nanos) in [
            ("1970-01-01T09:00:00.123456789+09:00", 123_456_789),
            ("1970-01-01T00:00:00.1234567Z", 123_456_700),
            ("1970-01-01T00:00:00.12345678Z", 123_456_780),
            ("1969-12-31T23:59:59.999999999Z", -1),
            ("1969-12-31T23:59:59.9999999Z", -100),
        ] {
            match exact(nanos) {
                Some(expected) => {
                    assert_eq!(parse_bucket_timestamp(input).unwrap(), expected, "{input}");
                }
                // Unrepresentable digits are rejected rather than rounded.
                None => assert!(parse_bucket_timestamp(input).is_err(), "{input}"),
            }
            assert!(parse_timestamp(input).is_err(), "{input}");
        }
        assert!(
            exact(-100).is_some(),
            "every platform holds 100 ns intervals"
        );
        for input in [
            "0000-01-01T00:00:00.000000001Z",
            "9999-12-31T23:59:59.999999999Z",
        ] {
            // Windows SystemTime starts in 1601 and holds 100 ns intervals.
            assert_eq!(parse_bucket_timestamp(input).is_ok(), cfg!(unix), "{input}");
        }
        for input in [
            "1970-01-01T00:00:00.0000000001Z",
            "0000-01-01T00:00:00+00:01",
            "9999-12-31T23:59:59-00:01",
        ] {
            assert!(parse_bucket_timestamp(input).is_err(), "{input}");
        }
    }
    #[test]
    fn retention_timestamps_keep_every_nanosecond_on_every_platform() {
        let at = |secs, nanos| crate::object::unix_nanos::UnixNanos::new(secs, nanos).unwrap();
        for (input, expected) in [
            (
                "2030-01-01T00:00:00.123456789Z",
                at(1_893_456_000, 123_456_789),
            ),
            (
                "2030-01-01T00:00:00.123456788Z",
                at(1_893_456_000, 123_456_788),
            ),
            (
                "2030-01-01T09:00:00.5+09:00",
                at(1_893_456_000, 500_000_000),
            ),
            ("1969-12-31T23:59:59.999999999Z", at(-1, 999_999_999)),
        ] {
            assert_eq!(parse_retention_timestamp(input), Ok(expected), "{input}");
        }
        assert!(parse_retention_timestamp("2030-01-01T00:00:00.1234567891Z").is_err());
    }
    proptest! {
        #[test]
        fn escaped_names_round_trip(name in any::<String>()) {
            prop_assert_eq!(path_unescape(&path_escape(&name)).unwrap(), name);
        }
        #[test]
        fn valid_bucket_names_round_trip(name in "[a-z][a-z0-9-]{1,60}[a-z0-9]") {
            prop_assert!(crate::json::validate_bucket_name(&name).is_ok());
            prop_assert_eq!(path_unescape(&path_escape(&name)).unwrap(), name);
        }
        #[test]
        fn arbitrary_times_never_panic(input in any::<String>()) { let _ = parse_timestamp(&input); }
    }
    #[test]
    fn exact_times_offsets_zero_and_negative_epoch() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Ok(0));
        assert_eq!(
            parse_timestamp("1970-01-01T01:30:00.123456000+01:30"),
            Ok(123456)
        );
        assert_eq!(parse_timestamp("1969-12-31T23:59:59.999999Z"), Ok(-1));
        assert!(is_unset_time("0001-01-01T00:00:00Z").unwrap());
        for input in [
            "2025-02-29T00:00:00Z",
            "1970-01-01T00:00:00.123456001Z",
            "1970-01-01T24:00:00Z",
            "1970-01-01T00:00:00Zx",
            "9999-12-31T23:59:59-01:00",
            "0000-01-01T00:00:00+01:00",
        ] {
            assert!(parse_timestamp(input).is_err(), "{input}");
        }
        assert_eq!(path_escape("a/b;c,d?%+"), "a%2Fb%3Bc%2Cd%3F%25+");
    }

    #[test]
    fn decoding_errors_keep_diagnostics_and_causes() {
        let error = path_unescape("%ff").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "decoded bucket must be UTF-8; restore the original snapshot"
        );
        assert!(
            error
                .get_ref()
                .unwrap()
                .source()
                .unwrap()
                .is::<std::string::FromUtf8Error>()
        );
        let error = path_unescape("%f").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "malformed bucket percent escape; restore the escaped directory name"
        );
        let error = super::super::reader::ImportFailure::corrupt(
            "timestamp",
            crate::storage::failure::BackendCause::from_error(
                parse_timestamp("invalid").unwrap_err(),
            ),
            "Restore a valid timestamp.",
        );
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<TimestampError>()
        );
    }
}
