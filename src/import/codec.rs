#![deny(clippy::unwrap_used, clippy::expect_used)]

use crate::time_format::{TimestampError, parse_nanosecond_timestamp, parse_timestamp};
use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, Visitor},
};
use std::{collections::BTreeMap, fmt};

/// A valid timestamp that this platform's `SystemTime` cannot hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("source timestamp is outside this platform's range")]
pub(super) struct OutsidePlatformRange;

/// A source value this codec cannot convert. Callers report its message; only the codec's own
/// tests look at which conversion failed.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct CodecError(Kind);

#[derive(Debug, thiserror::Error)]
enum Kind {
    #[error(transparent)]
    Timestamp(TimestampError),
    #[error(transparent)]
    PlatformRange(OutsidePlatformRange),
    /// A bucket timestamp outside this platform's clock; the inner error names the range.
    #[error("bucket timestamp is outside this platform's range")]
    BucketTimeRange(#[source] OutsidePlatformRange),
    /// The nanoseconds past the microsecond carry a bucket timestamp past this platform's clock.
    #[error("bucket timestamp is outside this platform's range")]
    BucketNanosRange,
    #[error("bucket timestamp is finer than this platform's clock resolution")]
    BucketResolution,
    /// Unreachable: the parser keeps the remainder below a microsecond. The conversion still
    /// names a failure because this module denies `expect`.
    #[error("retention timestamp is outside the supported range")]
    RetentionRange,
    #[error(transparent)]
    Acl(crate::acl::AclError),
    #[error("ACL auxiliary identity fields cannot be preserved; retain the source snapshot")]
    AclIdentity,
    #[error("source retention mode must be Locked or Unlocked")]
    RetentionMode,
    #[error("source retention mode requires a nonzero retain-until time")]
    RetentionUntil,
    #[error("malformed bucket percent escape; restore the escaped directory name")]
    Escape,
    #[error("decoded bucket must be UTF-8; restore the original snapshot")]
    Utf8(#[source] std::string::FromUtf8Error),
}

impl From<TimestampError> for CodecError {
    fn from(error: TimestampError) -> Self {
        Self(Kind::Timestamp(error))
    }
}

impl From<OutsidePlatformRange> for CodecError {
    fn from(error: OutsidePlatformRange) -> Self {
        Self(Kind::PlatformRange(error))
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
) -> Result<Option<Vec<crate::acl::AclEntry>>, CodecError> {
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
            return Err(CodecError(Kind::AclIdentity));
        }
        entries.push(crate::acl::AclEntryInput {
            entity: source.entity.clone(),
            role: source.role.clone(),
        });
    }
    crate::acl::parse_entries(kind, entries)
        .map(Some)
        .map_err(|error| CodecError(Kind::Acl(error)))
}

pub(super) fn retention(
    attrs: &ObjectAttrs,
) -> Result<Option<crate::object::retention::ObjectRetention>, CodecError> {
    let unset = is_unset_time(&attrs.retention_retain_until)?;
    if attrs.retention_mode.is_empty() && unset {
        return Ok(None);
    }
    let mode = crate::object::retention::RetentionMode::parse(&attrs.retention_mode)
        .ok_or(CodecError(Kind::RetentionMode))?;
    if unset {
        return Err(CodecError(Kind::RetentionUntil));
    }
    // Nanosecond precision, like the JSON adapter: retention times compare exactly.
    let retain_until = parse_retention_timestamp(&attrs.retention_retain_until)?;
    Ok(Some(crate::object::retention::ObjectRetention {
        mode,
        retain_until,
    }))
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "PascalCase")]
pub(crate) struct BucketAttrs {
    pub(crate) default_event_based_hold: bool,
    #[serde(
        deserialize_with = "present_bool",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) versioning_enabled: Option<bool>,
    #[serde(rename = "ACL")]
    pub(crate) acl: Option<Vec<SourceAcl>>,
}

fn present_bool<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<bool>, D::Error> {
    bool::deserialize(deserializer).map(Some)
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "PascalCase")]
pub(crate) struct ObjectAttrs {
    pub(crate) storage_class: String,
    pub(crate) content_type: String,
    pub(crate) content_encoding: String,
    pub(crate) content_disposition: String,
    pub(crate) content_language: String,
    pub(crate) cache_control: String,
    pub(crate) crc32c: String,
    pub(crate) md5_hash: String,
    pub(crate) etag: String,
    #[serde(rename = "ACL")]
    pub(crate) acl: Option<Vec<SourceAcl>>,
    #[serde(deserialize_with = "unique_metadata")]
    pub(crate) metadata: Option<BTreeMap<String, String>>,
    pub(crate) created: String,
    pub(crate) deleted: String,
    pub(crate) updated: String,
    pub(crate) custom_time: String,
    pub(crate) generation: i64,
    pub(crate) retention_mode: String,
    pub(crate) retention_retain_until: String,
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

pub(crate) fn path_unescape(name: &str) -> Result<String, CodecError> {
    use crate::percent::{Decoding, decode_bytes};
    let bytes = decode_bytes(name, Decoding::Path).map_err(|_| CodecError(Kind::Escape))?;
    String::from_utf8(bytes).map_err(|error| CodecError(Kind::Utf8(error)))
}

/// A retention time to the nanosecond, as seconds and nanoseconds. It never passes through a
/// `SystemTime`, so a platform that floors to 100 ns still keeps every digit.
pub(super) fn parse_retention_timestamp(
    input: &str,
) -> Result<crate::object::unix_nanos::UnixNanos, CodecError> {
    let (micros, nanos) = parse_nanosecond_timestamp(input)?;
    crate::object::unix_nanos::UnixNanos::from_micros_and_remainder(micros, nanos)
        .ok_or(CodecError(Kind::RetentionRange))
}

pub(super) fn parse_bucket_timestamp(input: &str) -> Result<std::time::SystemTime, CodecError> {
    let (micros, nanos) = parse_nanosecond_timestamp(input)?;
    let base = super::filesystem::time(micros)
        .map_err(|error| CodecError(Kind::BucketTimeRange(error)))?;
    let remainder = std::time::Duration::from_nanos(u64::from(nanos));
    let time = base
        .checked_add(remainder)
        .ok_or(CodecError(Kind::BucketNanosRange))?;
    // Windows keeps 100 ns intervals, so sub-interval digits would be dropped silently;
    // reject them like other unrepresentable source precision.
    if time.duration_since(base).ok() != Some(remainder) {
        return Err(CodecError(Kind::BucketResolution));
    }
    Ok(time)
}

pub(crate) fn is_unset_time(input: &str) -> Result<bool, TimestampError> {
    Ok(input.is_empty() || parse_timestamp(input)? == crate::clock::YEAR_ONE_MICROS)
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
            std::assert_matches!(error.0, Kind::Acl(ref error) if *error == expected,
                "{count} {entity}: {error:?}"
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
            "REGIONAL",
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
            assert_eq!(
                parse_retention_timestamp(input).unwrap(),
                expected,
                "{input}"
            );
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
            prop_assert!(crate::bucket::name::validate(&name).is_ok());
            prop_assert_eq!(path_unescape(&path_escape(&name)).unwrap(), name);
        }
        #[test]
        fn publishable_import_custom_times_match_the_object_parser(
            date in prop_oneof![
                Just((1, 1, 1)),
                Just((1969, 12, 31)),
                Just((2000, 2, 29)),
                Just((9999, 12, 31)),
                (1..=9999, 1..=12, 1..=31),
            ],
            time in (0..=23, 0..=59, 0..=59),
            digits in "[0-9]{0,6}0{0,3}",
            zone in prop_oneof![Just("Z".to_owned()), "[+-][0-2][0-9]:[0-5][0-9]"],
        ) {
            let (year, month, day) = date;
            let (hour, minute, second) = time;
            let date = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
            let input = if digits.is_empty() {
                format!("{date}{zone}")
            } else {
                format!("{date}.{digits}{zone}")
            };
            if is_unset_time(&input) == Ok(false) {
                let micros = parse_timestamp(&input).unwrap();
                let time = crate::object::CustomTime::parse(input.clone());
                prop_assert!(time.is_ok(), "{input}: {time:?}");
                let time = time.unwrap();
                prop_assert_eq!(time.as_str(), input);
                prop_assert_eq!(time.instant().unwrap().micros_and_remainder(), Some((micros, 0)));
            }
        }
    }

    #[test]
    fn import_preflight_accepts_some_custom_times_that_publication_rejects() {
        use crate::{
            metadata_error::MetadataError,
            object::{ObjectError, ObjectMetadataUpdate, Patch},
        };

        for input in ["1969-12-31T23:59:59.0000000000Z", "0000-12-31T23:59:59Z"] {
            assert_eq!(is_unset_time(input), Ok(false), "{input}");
            let update = ObjectMetadataUpdate {
                custom_time: Patch::Set(input.into()),
                ..Default::default()
            };
            std::assert_matches!(
                update.validate(),
                Err(ObjectError::Metadata(MetadataError::CustomTime)),
                "{input}"
            );
        }
    }

    #[test]
    fn the_go_zero_time_is_unset() {
        assert!(is_unset_time("0001-01-01T00:00:00Z").unwrap());
    }

    #[test]
    fn names_escape_like_go_path_escape() {
        assert_eq!(path_escape("a/b;c,d?%+"), "a%2Fb%3Bc%2Cd%3F%25+");
    }

    #[test]
    fn decoding_errors_keep_diagnostics_and_causes() {
        let error = path_unescape("%ff").unwrap_err();
        std::assert_matches!(error.0, Kind::Utf8(_), "{error:?}");
        assert_eq!(
            error.to_string(),
            "decoded bucket must be UTF-8; restore the original snapshot"
        );
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<std::string::FromUtf8Error>()
        );
        let error = path_unescape("%f").unwrap_err();
        std::assert_matches!(error.0, Kind::Escape, "{error:?}");
        assert_eq!(
            error.to_string(),
            "malformed bucket percent escape; restore the escaped directory name"
        );
        let error = super::super::reader::ImportFailure::corrupt(
            super::super::CorruptField::Snapshot,
            crate::cause::BackendCause::from_error(parse_timestamp("invalid").unwrap_err()),
            "Restore a valid timestamp.",
        );
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<TimestampError>()
        );
    }

    /// The journal stores the outer text, and diagnostics show the whole chain, so a bucket
    /// timestamp this platform cannot hold keeps naming both the bucket and the platform range.
    #[test]
    fn a_bucket_timestamp_outside_the_platform_keeps_its_text_and_cause() {
        const OUTER: &str = "bucket timestamp is outside this platform's range";
        const CHAIN: &str = "bucket timestamp is outside this platform's range: source timestamp is outside this platform's range";
        let error = CodecError(Kind::BucketTimeRange(OutsidePlatformRange));
        assert_eq!(error.to_string(), OUTER);
        assert_eq!(crate::error::Report::chain(&error).to_string(), CHAIN);
        // Windows SystemTime starts in 1601, so only it reaches this through parsing.
        #[cfg(not(unix))]
        {
            let error = parse_bucket_timestamp("0000-01-01T00:00:00Z").unwrap_err();
            std::assert_matches!(error.0, Kind::BucketTimeRange(_), "{error:?}");
            assert_eq!(crate::error::Report::chain(&error).to_string(), CHAIN);
        }
    }
}
