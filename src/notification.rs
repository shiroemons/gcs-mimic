//! Bucket Pub/Sub notification configurations shared by the JSON adapter and the catalog.
//!
//! Configurations feed a durable outbox; delivery requires an explicit local Pub/Sub endpoint.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

mod event;
pub(crate) mod publisher;
mod topic;

#[derive(Debug, thiserror::Error)]
#[error("invalid stored notification")]
pub(crate) enum NotificationDecodeError {
    Json(#[source] serde_json::Error),
    Shape,
    Invalid(#[source] NotificationError),
}

impl From<NotificationDecodeError> for crate::cause::BackendCause {
    fn from(error: NotificationDecodeError) -> Self {
        Self::from_error(error)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum NotificationMode {
    #[default]
    Emit,
    Suppress,
}

impl NotificationMode {
    pub(crate) fn for_access(access: &crate::bucket::BucketAccess) -> Self {
        match access {
            crate::bucket::BucketAccess::Admin(grant) => grant.notification_mode(),
            crate::bucket::BucketAccess::Permissive
            | crate::bucket::BucketAccess::Policy { .. } => Self::Emit,
        }
    }
}

pub(crate) use event::{Event, EventTypes, Message};
pub(crate) use publisher::{Endpoint, TopicCheckError};
pub(crate) use topic::Topic;

#[derive(Clone, Debug, Default)]
pub(crate) struct Runtime {
    pub(crate) endpoint: Option<Endpoint>,
    pub(crate) global: Option<GlobalConfig>,
}

#[derive(Clone, Debug)]
pub(crate) struct GlobalConfig {
    pub(crate) bucket: String,
    pub(crate) notification: NotificationConfig,
}

/// Documented per-bucket total, per-event-type and custom-attribute limits.
pub(crate) const MAX_PER_BUCKET: usize = 100;
pub(crate) const MAX_PER_EVENT_TYPE: usize = 10;
pub(crate) const MAX_CUSTOM_ATTRIBUTES: usize = 10;

crate::choice::string_enum! {
    table
    /// What a notification message carries besides its attributes.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) enum PayloadFormat as pub(crate) as_str {
        /// The object's JSON API resource, which the API defaults to.
        #[default]
        JsonApiV1 => "JSON_API_V1",
        None => "NONE",
    };
    from_str(NotificationError, value => Err(NotificationError::InvalidPayloadFormat(value.to_owned())))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotificationConfig {
    pub(crate) topic: Topic,
    pub(crate) payload_format: PayloadFormat,
    pub(crate) event_types: EventTypes,
    pub(crate) custom_attributes: BTreeMap<String, String>,
    /// Empty means every object name.
    pub(crate) object_name_prefix: String,
}

/// A rejected notification configuration. `Display` is the client-visible JSON error message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum NotificationError {
    #[error(
        "Invalid topic {0:?}; use //pubsub.googleapis.com/projects/PROJECT/topics/TOPIC (for example, //pubsub.googleapis.com/projects/local-project/topics/uploads)"
    )]
    InvalidTopic(String),
    #[error("Invalid payload_format {0:?}; use JSON_API_V1 or NONE")]
    InvalidPayloadFormat(String),
    #[error(
        "Invalid event type {0:?}; use OBJECT_FINALIZE, OBJECT_METADATA_UPDATE, OBJECT_DELETE or OBJECT_ARCHIVE"
    )]
    InvalidEventType(String),
    #[error("Duplicate event type {0}; list each event type once")]
    DuplicateEventType(Event),
    #[error(
        "custom_attributes has {0} entries; a notification allows at most {MAX_CUSTOM_ATTRIBUTES}"
    )]
    TooManyCustomAttributes(usize),
    #[error("Invalid custom attribute key {0:?}; use 1 to 256 bytes that do not start with goog")]
    InvalidCustomAttributeKey(String),
    #[error("Custom attribute {0:?} exceeds 1024 bytes")]
    CustomAttributeTooLong(String),
    #[error("object_name_prefix exceeds 1024 bytes")]
    ObjectNamePrefixTooLong,
    #[error(
        "Bucket already has {MAX_PER_BUCKET} notification configurations; delete one before adding another"
    )]
    BucketLimit,
    #[error(
        "Bucket already has {MAX_PER_EVENT_TYPE} notification configurations for {0}; delete one or narrow event_types"
    )]
    EventTypeLimit(Event),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotificationRecord {
    pub(crate) id: i64,
    pub(crate) config: NotificationConfig,
}

impl NotificationConfig {
    pub(crate) fn to_stored(&self) -> String {
        // serde_json's preserve_order keeps this member order, which stored rows have always had.
        Value::Object(Map::from_iter([
            ("topic".into(), Value::from(self.topic.as_str())),
            (
                "payload_format".into(),
                Value::from(self.payload_format.as_str()),
            ),
            ("event_types".into(), Value::from(&self.event_types)),
            (
                "custom_attributes".into(),
                crate::serde_util::string_map(&self.custom_attributes),
            ),
            (
                "object_name_prefix".into(),
                Value::from(self.object_name_prefix.as_str()),
            ),
        ]))
        .to_string()
    }

    /// Re-validate stored rows so a corrupt catalog cannot render configurations the API rejects.
    pub(crate) fn from_stored(stored: &str) -> Result<Self, NotificationDecodeError> {
        use NotificationDecodeError::Shape;
        let value: Value = serde_json::from_str(stored).map_err(NotificationDecodeError::Json)?;
        let value = value.as_object().ok_or(Shape)?;
        let string = |key: &str| value.get(key).and_then(Value::as_str).ok_or(Shape);
        let event_types = value
            .get("event_types")
            .ok_or(Shape)?
            .as_array()
            .ok_or(Shape)?
            .iter()
            .map(|event| event.as_str().ok_or(Shape))
            .collect::<Result<Vec<_>, _>>()?;
        let custom_attributes = value
            .get("custom_attributes")
            .ok_or(Shape)?
            .as_object()
            .ok_or(Shape)?
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_owned()))
                    .ok_or(Shape)
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let topic = string("topic")?;
        let payload_format = string("payload_format")?;
        let object_name_prefix = string("object_name_prefix")?;
        // Only the shape is checked above; the values must also be ones the API accepts.
        let invalid = NotificationDecodeError::Invalid;
        let parsed = topic.parse::<Topic>().map_err(invalid)?;
        // This server stores only the full form, so a relative topic was not written by it.
        if parsed.as_str() != topic {
            return Err(Shape);
        }
        let config = Self {
            topic: parsed,
            payload_format: payload_format.parse().map_err(invalid)?,
            event_types: EventTypes::parse(event_types).map_err(invalid)?,
            custom_attributes,
            object_name_prefix: object_name_prefix.to_owned(),
        };
        validate_custom_attributes(&config.custom_attributes).map_err(invalid)?;
        validate_object_name_prefix(&config.object_name_prefix).map_err(invalid)?;
        Ok(config)
    }
}

pub(crate) fn validate_custom_attributes(
    attributes: &BTreeMap<String, String>,
) -> Result<(), NotificationError> {
    if attributes.len() > MAX_CUSTOM_ATTRIBUTES {
        return Err(NotificationError::TooManyCustomAttributes(attributes.len()));
    }
    // Pub/Sub message attribute bounds: these values are copied onto every published message.
    for (key, value) in attributes {
        if key.is_empty() || key.len() > 256 || key.starts_with("goog") {
            return Err(NotificationError::InvalidCustomAttributeKey(key.clone()));
        }
        if value.len() > 1024 {
            return Err(NotificationError::CustomAttributeTooLong(key.clone()));
        }
    }
    Ok(())
}

pub(crate) fn validate_object_name_prefix(prefix: &str) -> Result<(), NotificationError> {
    // A prefix longer than the object-name limit could never match an object.
    if prefix.len() > 1024 {
        return Err(NotificationError::ObjectNamePrefixTooLong);
    }
    Ok(())
}

/// Check the documented per-bucket and per-event-type limits before adding `candidate`.
pub(crate) fn check_limits(
    existing: &[NotificationConfig],
    candidate: &NotificationConfig,
) -> Result<(), NotificationError> {
    if existing.len() >= MAX_PER_BUCKET {
        return Err(NotificationError::BucketLimit);
    }
    for event in candidate.event_types.effective() {
        let count = existing
            .iter()
            .filter(|config| config.event_types.selects(event))
            .count();
        if count >= MAX_PER_EVENT_TYPE {
            return Err(NotificationError::EventTypeLimit(event));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_enum_spellings_and_refusals_are_stable() {
        for (format, spelling) in [
            (PayloadFormat::JsonApiV1, "JSON_API_V1"),
            (PayloadFormat::None, "NONE"),
        ] {
            assert_eq!(format.as_str(), spelling);
            assert_eq!(spelling.parse::<PayloadFormat>().unwrap(), format);
        }
        assert_eq!(
            "none".parse::<PayloadFormat>().unwrap_err().to_string(),
            "Invalid payload_format \"none\"; use JSON_API_V1 or NONE"
        );
        let spellings = [
            "OBJECT_FINALIZE",
            "OBJECT_METADATA_UPDATE",
            "OBJECT_DELETE",
            "OBJECT_ARCHIVE",
        ];
        assert_eq!(Event::ALL.len(), spellings.len());
        for (&event, spelling) in Event::ALL.iter().zip(spellings) {
            assert_eq!(event.as_str(), spelling);
            assert_eq!(spelling.parse::<Event>().unwrap(), event);
        }
        assert_eq!(
            "object_delete".parse::<Event>().unwrap_err().to_string(),
            "Invalid event type \"object_delete\"; use OBJECT_FINALIZE, OBJECT_METADATA_UPDATE, OBJECT_DELETE or OBJECT_ARCHIVE"
        );
    }

    fn config(events: &[&str]) -> NotificationConfig {
        NotificationConfig {
            topic: "//pubsub.googleapis.com/projects/p/topics/topic"
                .parse()
                .unwrap(),
            payload_format: PayloadFormat::JsonApiV1,
            event_types: EventTypes::parse(events.iter().copied()).unwrap(),
            custom_attributes: BTreeMap::new(),
            object_name_prefix: String::new(),
        }
    }

    #[test]
    fn topics_normalize_relative_and_full_names_and_reject_malformed_names() {
        for topic in [
            "projects/local-project/topics/uploads",
            "//pubsub.googleapis.com/projects/local-project/topics/uploads",
        ] {
            assert_eq!(
                topic.parse::<Topic>().unwrap().as_str(),
                "//pubsub.googleapis.com/projects/local-project/topics/uploads"
            );
        }
        assert_eq!(
            "projects/example.com:p/topics/a.b~c+d%e_f-g"
                .parse::<Topic>()
                .unwrap()
                .as_str(),
            "//pubsub.googleapis.com/projects/example.com:p/topics/a.b~c+d%e_f-g"
        );
        let longest = format!("projects/p/topics/t{}", "a".repeat(254));
        assert!(longest.parse::<Topic>().is_ok());
        for topic in [
            "",
            "uploads",
            "projects/p/topics",
            "projects/p/topics/",
            "projects//topics/abc",
            "projects/p/subscriptions/abc",
            "projects/p/topics/abc/extra",
            "//pubsub.googleapis.com/projects/p/topics/ab",
            "https://pubsub.googleapis.com/projects/p/topics/abc",
            "projects/p/topics/1abc",
            "projects/p/topics/google",
            "projects/p/topics/a b c",
            "projects/p q/topics/abc",
            &format!("projects/p/topics/t{}", "a".repeat(255)),
        ] {
            assert!(topic.parse::<Topic>().is_err(), "{topic}");
        }
    }

    #[test]
    fn fields_reject_unknown_duplicate_and_oversized_values() {
        assert_eq!("JSON_API_V1".parse(), Ok(PayloadFormat::JsonApiV1));
        assert_eq!("NONE".parse(), Ok(PayloadFormat::None));
        assert!("json_api_v1".parse::<PayloadFormat>().is_err());
        assert!(EventTypes::parse(["OBJECT_FINALIZE", "OBJECT_ARCHIVE"]).is_ok());
        assert!(EventTypes::parse(["OBJECT_CREATE"]).is_err());
        assert!(EventTypes::parse(["OBJECT_DELETE", "OBJECT_DELETE"]).is_err());
        let mut attributes: BTreeMap<String, String> = (0..10)
            .map(|index| (format!("k{index}"), "v".repeat(1024)))
            .collect();
        assert!(validate_custom_attributes(&attributes).is_ok());
        attributes.insert("k10".into(), String::new());
        assert!(validate_custom_attributes(&attributes).is_err());
        for (key, value) in [
            (String::new(), String::new()),
            ("googKey".into(), String::new()),
            ("k".repeat(257), String::new()),
            ("k".into(), "v".repeat(1025)),
        ] {
            assert!(validate_custom_attributes(&BTreeMap::from([(key, value)])).is_err());
        }
        assert!(validate_object_name_prefix(&"p".repeat(1024)).is_ok());
        assert!(validate_object_name_prefix(&"p".repeat(1025)).is_err());
    }

    #[test]
    fn limits_count_all_event_configs_against_every_event_type() {
        let mut existing = vec![config(&[]); 9];
        assert!(check_limits(&existing, &config(&["OBJECT_DELETE"])).is_ok());
        existing.push(config(&["OBJECT_FINALIZE"]));
        assert!(check_limits(&existing, &config(&["OBJECT_FINALIZE"])).is_err());
        assert!(check_limits(&existing, &config(&[])).is_err());
        assert!(check_limits(&existing, &config(&["OBJECT_DELETE"])).is_ok());
        let full: Vec<_> = Event::ALL
            .iter()
            .flat_map(|event| vec![config(&[event.as_str()]); 10])
            .collect();
        assert!(check_limits(&full, &config(&["OBJECT_ARCHIVE"])).is_err());
        let at_total: Vec<_> = (0..MAX_PER_BUCKET)
            .map(|_| config(&["OBJECT_FINALIZE"]))
            .collect();
        assert!(check_limits(&at_total, &config(&["OBJECT_DELETE"])).is_err());
    }

    #[test]
    fn stored_configs_round_trip_and_reject_invalid_rows() {
        let mut original = config(&["OBJECT_DELETE"]);
        original
            .custom_attributes
            .insert("team".into(), "storage".into());
        original.object_name_prefix = "logs/".into();
        assert_eq!(
            NotificationConfig::from_stored(&original.to_stored()).unwrap(),
            original
        );
        for stored in [
            "{}",
            "[]",
            r#"{"topic":"projects/p/topics/topic","payload_format":"NONE","event_types":[],"custom_attributes":{},"object_name_prefix":""}"#,
            r#"{"topic":"//pubsub.googleapis.com/projects/p/topics/topic","payload_format":"XML","event_types":[],"custom_attributes":{},"object_name_prefix":""}"#,
        ] {
            assert!(NotificationConfig::from_stored(stored).is_err(), "{stored}");
        }
    }

    /// The catalog keeps this text in `bucket_notifications.config`, so its keys, their order
    /// and the order of event types must not change.
    #[test]
    fn stored_configs_keep_their_bytes() {
        let mut config = config(&["OBJECT_DELETE", "OBJECT_FINALIZE"]);
        config.payload_format = PayloadFormat::None;
        config.custom_attributes.insert("b".into(), "2".into());
        config.custom_attributes.insert("a".into(), "1".into());
        config.object_name_prefix = "logs/".into();
        assert_eq!(
            config.to_stored(),
            r#"{"topic":"//pubsub.googleapis.com/projects/p/topics/topic","payload_format":"NONE","event_types":["OBJECT_DELETE","OBJECT_FINALIZE"],"custom_attributes":{"a":"1","b":"2"},"object_name_prefix":"logs/"}"#
        );
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let cases: &[(&dyn std::error::Error, &str)] = &[
        (
            &NotificationError::InvalidTopic("bad".into()),
            "Invalid topic \"bad\"; use //pubsub.googleapis.com/projects/PROJECT/topics/TOPIC (for example, //pubsub.googleapis.com/projects/local-project/topics/uploads)",
        ),
        (
            &NotificationError::InvalidEventType("bad".into()),
            "Invalid event type \"bad\"; use OBJECT_FINALIZE, OBJECT_METADATA_UPDATE, OBJECT_DELETE or OBJECT_ARCHIVE",
        ),
        (
            &NotificationError::DuplicateEventType(Event::Finalize),
            "Duplicate event type OBJECT_FINALIZE; list each event type once",
        ),
        (
            &NotificationError::TooManyCustomAttributes(11),
            "custom_attributes has 11 entries; a notification allows at most 10",
        ),
        (
            &NotificationError::InvalidCustomAttributeKey("bad".into()),
            "Invalid custom attribute key \"bad\"; use 1 to 256 bytes that do not start with goog",
        ),
        (
            &NotificationError::CustomAttributeTooLong("key".into()),
            "Custom attribute \"key\" exceeds 1024 bytes",
        ),
        (
            &NotificationError::ObjectNamePrefixTooLong,
            "object_name_prefix exceeds 1024 bytes",
        ),
        (
            &NotificationError::BucketLimit,
            "Bucket already has 100 notification configurations; delete one before adding another",
        ),
        (
            &NotificationError::EventTypeLimit(Event::Delete),
            "Bucket already has 10 notification configurations for OBJECT_DELETE; delete one or narrow event_types",
        ),
        (
            &NotificationDecodeError::Shape,
            "invalid stored notification",
        ),
        (
            &NotificationError::InvalidPayloadFormat("X".into()),
            "Invalid payload_format \"X\"; use JSON_API_V1 or NONE",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), *expected);
        assert!(error.source().is_none());
    }
    let error = NotificationDecodeError::Invalid(NotificationError::BucketLimit);
    assert_eq!(error.to_string(), "invalid stored notification");
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<NotificationError>()
    );
}
