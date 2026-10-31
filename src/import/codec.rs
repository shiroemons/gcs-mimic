#![deny(clippy::unwrap_used, clippy::expect_used)]

use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, Visitor},
};
use std::{collections::BTreeMap, fmt};

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
        entries.push(crate::acl::AclEntry {
            entity: source.entity.clone(),
            role: source.role.clone(),
        });
    }
    crate::acl::validate_entries(kind, &entries)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(Some(entries))
}

pub(super) fn retention(
    attrs: &ObjectAttrs,
) -> Result<Option<crate::object::retention::ObjectRetention>, std::io::Error> {
    let invalid = |reason| std::io::Error::new(std::io::ErrorKind::InvalidData, reason);
    let unset = is_unset_time(&attrs.retention_retain_until).map_err(invalid)?;
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
    let micros = parse_timestamp(&attrs.retention_retain_until).map_err(invalid)?;
    Ok(Some(crate::object::retention::ObjectRetention {
        mode,
        retain_until: super::filesystem::time(micros)?,
    }))
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "PascalCase")]
pub(crate) struct BucketAttrs {
    pub default_event_based_hold: bool,
    pub versioning_enabled: bool,
    #[serde(rename = "ACL")]
    pub acl: Option<Vec<SourceAcl>>,
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

pub(crate) fn path_unescape(name: &str) -> Result<String, &'static str> {
    let mut result = Vec::with_capacity(name.len());
    let mut bytes = name.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|b| char::from(b).to_digit(16));
            let low = bytes.next().and_then(|b| char::from(b).to_digit(16));
            match (high, low) {
                (Some(high), Some(low)) => result.push((high * 16 + low) as u8),
                _ => {
                    return Err(
                        "malformed bucket percent escape; restore the escaped directory name",
                    );
                }
            }
        } else {
            result.push(byte);
        }
    }
    String::from_utf8(result)
        .map_err(|_| "decoded bucket must be UTF-8; restore the original snapshot")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Precision {
    Micros,
    Nanos,
}

pub(crate) fn parse_timestamp(input: &str) -> Result<i64, &'static str> {
    parse_timestamp_parts(input, Precision::Micros).map(|(micros, _)| micros)
}

pub(super) fn parse_bucket_timestamp(input: &str) -> Result<std::time::SystemTime, &'static str> {
    let (micros, nanos) = parse_timestamp_parts(input, Precision::Nanos)?;
    super::filesystem::time(micros)
        .map_err(|_| "bucket timestamp is outside this platform's range")?
        .checked_add(std::time::Duration::from_nanos(u64::from(nanos)))
        .ok_or("bucket timestamp is outside this platform's range")
}

fn parse_timestamp_parts(input: &str, precision: Precision) -> Result<(i64, u32), &'static str> {
    let invalid = "invalid source timestamp; supply a complete RFC3339 source timestamp";
    let bytes = input.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(invalid);
    }
    let number = |start, end| -> Result<i64, &'static str> {
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
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            let digit = i64::from(bytes[index] - b'0');
            if index - start < 6 {
                micros = micros * 10 + digit;
            } else if index - start < 9 && precision == Precision::Nanos {
                nanos = nanos * 10 + digit as u32;
            } else if digit != 0 {
                if precision == Precision::Nanos {
                    return Err(
                        "nonzero sub-nanosecond timestamp digits; retain the exact source timestamp without truncation",
                    );
                }
                return Err(
                    "nonzero sub-microsecond timestamp digits; use an exact-microsecond snapshot, without truncation",
                );
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
    } else if zone.len() == 6 && matches!(zone[0], b'+' | b'-') && zone[3] == b':' {
        let zh = number(index + 1, index + 3)?;
        let zm = number(index + 4, index + 6)?;
        if zh > 23 || zm > 59 {
            return Err(invalid);
        }
        (zh * 3600 + zm * 60) * if zone[0] == b'-' { -1 } else { 1 }
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
        return Err(
            "timestamp is outside the supported RFC3339 UTC range after timezone normalization",
        );
    }
    Ok((micros, nanos))
}

pub(crate) fn is_unset_time(input: &str) -> Result<bool, &'static str> {
    Ok(input.is_empty() || parse_timestamp(input)? == -62_135_596_800_000_000)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn source_acl_duplicate_identity_fields_are_not_silently_overwritten() {
        assert!(serde_json::from_slice::<ObjectAttrs>(
            br#"{"ACL":[{"Entity":"allUsers","Entity":"allAuthenticatedUsers","Role":"READER"}]}"#
        ).is_err());
    }
    #[test]
    fn bucket_nanoseconds_preserve_negative_instants_without_relaxing_object_precision() {
        use std::time::{Duration, UNIX_EPOCH};
        for (input, expected) in [
            (
                "1970-01-01T09:00:00.123456789+09:00",
                UNIX_EPOCH + Duration::from_nanos(123_456_789),
            ),
            (
                "1970-01-01T00:00:00.1234567Z",
                UNIX_EPOCH + Duration::from_nanos(123_456_700),
            ),
            (
                "1970-01-01T00:00:00.12345678Z",
                UNIX_EPOCH + Duration::from_nanos(123_456_780),
            ),
            (
                "1969-12-31T23:59:59.999999999Z",
                UNIX_EPOCH - Duration::from_nanos(1),
            ),
        ] {
            assert_eq!(parse_bucket_timestamp(input).unwrap(), expected, "{input}");
            assert!(parse_timestamp(input).is_err(), "{input}");
        }
        for input in [
            "0000-01-01T00:00:00.000000001Z",
            "9999-12-31T23:59:59.999999999Z",
        ] {
            assert!(parse_bucket_timestamp(input).is_ok(), "{input}");
        }
        for input in [
            "1970-01-01T00:00:00.0000000001Z",
            "0000-01-01T00:00:00+00:01",
            "9999-12-31T23:59:59-00:01",
        ] {
            assert!(parse_bucket_timestamp(input).is_err(), "{input}");
        }
    }
    proptest! {
        #[test]
        fn escaped_names_round_trip(name in any::<String>()) {
            prop_assert_eq!(path_unescape(&path_escape(&name)), Ok(name));
        }
        #[test]
        fn valid_bucket_names_round_trip(name in "[a-z][a-z0-9-]{1,60}[a-z0-9]") {
            prop_assert!(crate::json::validate_bucket_name(&name).is_ok());
            prop_assert_eq!(path_unescape(&path_escape(&name)), Ok(name));
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
        assert!(path_unescape("%ff").is_err());
    }
}
