//! JSON wire form of object retention (`retention`) and bucket `objectRetention`.

use super::ApiError;
use crate::object::{
    retention::{ObjectRetention, RetentionMode},
    unix_nanos::UnixNanos,
};
use crate::time_format::{TimeOutOfRange, timestamp_nanos};
use serde_json::{Value, json};

const EXAMPLE: &str = "use {\"mode\":\"Unlocked\",\"retainUntilTime\":\"2030-01-01T00:00:00Z\"}";

/// Parses an object `retention` value. `null`, `{}` and an object whose fields are all null
/// remove the configuration: Go sends `null` and Ruby sends `{}` for removal.
pub(super) fn parse(value: &Value) -> Result<Option<ObjectRetention>, ApiError> {
    let invalid = |detail: &str| ApiError::invalid(format!("{detail}; {EXAMPLE}"));
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
            return Err(ApiError::invalid(
                "Missing retain until time. Both mode and retain until time are required.",
            ));
        }
    };
    let mode = mode
        .as_str()
        .and_then(RetentionMode::parse)
        .ok_or_else(|| {
            ApiError::invalid(format!(
                "Invalid value for: {} is not a valid value",
                mode.as_str().unwrap_or_default()
            ))
        })?;
    let until = until
        .as_str()
        .ok_or_else(|| invalid("retention.retainUntilTime must be an RFC 3339 string"))?;
    let retain_until = crate::object::custom_time_key(until)
        .map_err(|_| invalid("retention.retainUntilTime must be an RFC 3339 timestamp"))?;
    // Cloud Storage kept the full nanosecond value for comparisons while responses showed only
    // the millisecond (observed 2026-10-31, including .123999999 displayed as .123). The time is
    // kept to the nanosecond too, as seconds and nanoseconds rather than a `SystemTime`, which
    // Windows floors to 100 ns; rendering truncates to milliseconds.
    let out_of_range = || invalid("retention.retainUntilTime is outside the supported range");
    timestamp_nanos(retain_until).map_err(|_| out_of_range())?;
    Ok(Some(ObjectRetention { mode, retain_until }))
}

pub(super) fn render(retention: &ObjectRetention) -> Result<Value, TimeOutOfRange> {
    Ok(json!({
        "mode": retention.mode.as_str(),
        "retainUntilTime": timestamp_nanos(retention.retain_until)?,
    }))
}

pub(super) fn render_bucket(bucket: &crate::bucket::BucketRecord) -> Option<Value> {
    bucket
        .object_retention_enabled
        .then(|| json!({"mode": "Enabled"}))
}

/// Returned as `retentionExpirationTime`: the later of bucket-policy and object retention.
pub(super) fn expiration(time: Option<UnixNanos>) -> Result<Option<String>, TimeOutOfRange> {
    time.map(timestamp_nanos).transpose()
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
                parse(&invalid).unwrap_err().reason(),
                "invalid",
                "{invalid}"
            );
        }
    }

    fn parsed_render(time: &str) -> Result<String, ApiError> {
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
        // The sub-millisecond part is kept, so times one nanosecond apart still compare as
        // different on every platform (not a multiple of 100 ns, which Windows would floor).
        let (lower, higher) = (
            "2030-01-01T00:00:00.123456788Z",
            "2030-01-01T00:00:00.123456789Z",
        );
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
            parsed_render("10000-01-01T00:00:00Z").unwrap_err().reason(),
            "invalid"
        );
    }
}
