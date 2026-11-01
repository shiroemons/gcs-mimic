//! JSON wire form of object retention (`retention`) and bucket `objectRetention`.

use super::{JsonError, buckets::timestamp};
use crate::object::retention::{ObjectRetention, RetentionMode};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const EXAMPLE: &str = "use {\"mode\":\"Unlocked\",\"retainUntilTime\":\"2030-01-01T00:00:00Z\"}";

/// Parses an object `retention` value. `null`, `{}` and an object whose fields are all null
/// remove the configuration: Go sends `null` and Ruby sends `{}` for removal.
pub(super) fn parse(value: &Value) -> Result<Option<ObjectRetention>, JsonError> {
    let invalid = |detail: &str| JsonError::invalid_argument(format!("{detail}; {EXAMPLE}"));
    if value.is_null() {
        return Ok(None);
    }
    let fields = value
        .as_object()
        .ok_or_else(|| invalid("retention must be an object or null"))?;
    if let Some(key) = fields.keys().find(|key| {
        !matches!(key.as_str(), "mode" | "retainUntilTime")
            && !(key.as_str() == "retentionExpirationTime" && fields[*key].is_null())
    }) {
        return Err(invalid(&format!(
            "retention.{key} is not a retention field"
        )));
    }
    let mode = fields.get("mode").filter(|value| !value.is_null());
    let until = fields
        .get("retainUntilTime")
        .filter(|value| !value.is_null());
    let (mode, until) = match (mode, until) {
        (None, None) => return Ok(None),
        (Some(mode), Some(until)) => (mode, until),
        _ => {
            return Err(invalid("retention requires both mode and retainUntilTime"));
        }
    };
    let mode = mode
        .as_str()
        .and_then(RetentionMode::parse)
        .ok_or_else(|| invalid("retention.mode must be Unlocked or Locked"))?;
    let until = until
        .as_str()
        .ok_or_else(|| invalid("retention.retainUntilTime must be an RFC 3339 string"))?;
    let (seconds, nanos) = crate::object::custom_time_key(until)
        .map_err(|_| invalid("retention.retainUntilTime must be an RFC 3339 timestamp"))?;
    let offset = Duration::from_secs(seconds.unsigned_abs());
    let base = if seconds < 0 {
        UNIX_EPOCH.checked_sub(offset)
    } else {
        UNIX_EPOCH.checked_add(offset)
    };
    // Cloud Storage kept the full nanosecond value for comparisons while responses showed only
    // the millisecond (observed 2026-10-31, including .123999999 displayed as .123). The time is
    // kept to the nanosecond too and rendering truncates to milliseconds.
    let out_of_range = || invalid("retention.retainUntilTime is outside the supported range");
    let retain_until = base
        .and_then(|time| time.checked_add(Duration::from_nanos(u64::from(nanos))))
        .ok_or_else(out_of_range)?;
    timestamp(retain_until).map_err(|_| out_of_range())?;
    Ok(Some(ObjectRetention { mode, retain_until }))
}

pub(super) fn render(retention: &ObjectRetention) -> Result<Value, JsonError> {
    Ok(json!({
        "mode": retention.mode.as_str(),
        "retainUntilTime": timestamp(retention.retain_until)?,
    }))
}

/// Parses an echoed bucket `objectRetention`: `Some(true)` for `{"mode":"Enabled"}`.
///
/// The value is output-only after creation; callers compare it with the stored state.
pub(super) fn parse_bucket(value: Option<&Value>) -> Result<Option<bool>, JsonError> {
    let Some(value) = value.filter(|value| !value.is_null() && *value != &json!({})) else {
        return Ok(None);
    };
    match value.get("mode").and_then(Value::as_str) {
        Some("Enabled") if value.as_object().is_some_and(|fields| fields.len() == 1) => {
            Ok(Some(true))
        }
        _ => Err(JsonError::invalid_argument(
            "objectRetention only reports {\"mode\":\"Enabled\"}; enable object retention with the enableObjectRetention=true query parameter on bucket insert",
        )),
    }
}

pub(super) fn render_bucket(bucket: &crate::bucket::BucketRecord) -> Option<Value> {
    bucket
        .object_retention_enabled
        .then(|| json!({"mode": "Enabled"}))
}

/// Returned as `retentionExpirationTime`: the later of bucket-policy and object retention.
pub(super) fn expiration(time: Option<SystemTime>) -> Result<Option<String>, JsonError> {
    time.map(timestamp).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_retention_json_parsing_boundaries() {
        let parsed = parse(
            &json!({"mode":"Locked","retainUntilTime":"2030-01-02T03:04:05.123456789+09:00"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(parsed.mode, RetentionMode::Locked);
        assert_eq!(
            parse(&json!({"mode":"Locked","retainUntilTime":"2030-01-02T03:04:05.123456789+09:00","retentionExpirationTime":null})).unwrap(),
            Some(parsed)
        );
        assert_eq!(
            render(&parsed).unwrap(),
            json!({"mode":"Locked","retainUntilTime":"2030-01-01T18:04:05.123Z"})
        );
        for removal in [
            Value::Null,
            json!({}),
            json!({"mode":null,"retainUntilTime":null}),
        ] {
            assert_eq!(parse(&removal).unwrap(), None, "{removal}");
        }
        for invalid in [
            json!(true),
            json!({"mode":"Unlocked"}),
            json!({"retainUntilTime":"2030-01-01T00:00:00Z"}),
            json!({"mode":"GOVERNANCE","retainUntilTime":"2030-01-01T00:00:00Z"}),
            json!({"mode":"Unlocked","retainUntilTime":"tomorrow"}),
            json!({"mode":"Unlocked","retainUntilTime":1}),
            json!({"mode":"Unlocked","retainUntilTime":"2030-01-01T00:00:00Z","extra":1}),
            json!({"mode":"Unlocked","retainUntilTime":"2030-01-01T00:00:00Z","retentionExpirationTime":"2020-01-01T00:00:00Z"}),
        ] {
            assert_eq!(
                parse(&invalid).unwrap_err().reason,
                "invalidArgument",
                "{invalid}"
            );
        }
    }

    fn parsed_render(time: &str) -> Result<String, JsonError> {
        let parsed = parse(&json!({"mode":"Unlocked","retainUntilTime":time}))?.unwrap();
        Ok(render(&parsed)?["retainUntilTime"]
            .as_str()
            .unwrap()
            .to_owned())
    }

    #[test]
    fn retain_until_displays_milliseconds_and_keeps_nanoseconds() {
        assert_eq!(
            parsed_render("2030-01-01T00:00:00Z").unwrap(),
            "2030-01-01T00:00:00.000Z"
        );
        assert_eq!(
            parsed_render("2030-01-01T00:00:00.123Z").unwrap(),
            "2030-01-01T00:00:00.123Z"
        );
        // Cloud Storage displayed the millisecond truncation of a finer time (observed
        // 2026-10-31), never a rounded-up one.
        for fraction in ["123000001", "123456789", "123999999"] {
            assert_eq!(
                parsed_render(&format!("2030-01-01T00:00:00.{fraction}Z")).unwrap(),
                "2030-01-01T00:00:00.123Z",
                "{fraction}"
            );
        }
        let stored = |time: &str| {
            parse(&json!({"mode":"Unlocked","retainUntilTime":time}))
                .unwrap()
                .unwrap()
                .retain_until
        };
        // The sub-millisecond part is kept, so times one SystemTime tick apart still compare as
        // different. A tick is 1 ns on Unix but 100 ns on Windows, which rounds a finer fraction.
        let (lower, higher) = if cfg!(windows) {
            (
                "2030-01-01T00:00:00.123456700Z",
                "2030-01-01T00:00:00.123456800Z",
            )
        } else {
            (
                "2030-01-01T00:00:00.123456788Z",
                "2030-01-01T00:00:00.123456789Z",
            )
        };
        assert_ne!(stored(lower), stored(higher));
        assert!(stored(lower) < stored(higher));
    }

    #[test]
    fn retain_until_at_the_end_of_the_supported_range() {
        assert_eq!(
            parsed_render("9999-12-31T23:59:59.999999999Z").unwrap(),
            "9999-12-31T23:59:59.999Z"
        );
        assert_eq!(
            parsed_render("10000-01-01T00:00:00Z").unwrap_err().reason,
            "invalidArgument"
        );
    }
    #[test]
    fn bucket_object_retention_echo_values() {
        assert_eq!(parse_bucket(None).unwrap(), None);
        assert_eq!(parse_bucket(Some(&Value::Null)).unwrap(), None);
        assert_eq!(parse_bucket(Some(&json!({}))).unwrap(), None);
        assert_eq!(
            parse_bucket(Some(&json!({"mode":"Enabled"}))).unwrap(),
            Some(true)
        );
        for invalid in [
            json!({"mode":"Disabled"}),
            json!({"mode":"Enabled","extra":true}),
            json!(true),
        ] {
            assert!(parse_bucket(Some(&invalid)).is_err(), "{invalid}");
        }
    }
}
