//! Bucket Pub/Sub notification configurations shared by the JSON adapter and the catalog.
//!
//! Configurations feed a durable outbox; delivery requires an explicit local Pub/Sub endpoint.

use serde_json::{Map, Value};
use std::{collections::BTreeMap, fmt};

mod event;
pub(crate) mod publisher;
mod topic;

#[derive(Debug)]
pub(crate) enum NotificationDecodeError {
    Json(serde_json::Error),
    Shape,
    Invalid(NotificationError),
}

impl fmt::Display for NotificationDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid stored notification")
    }
}

impl std::error::Error for NotificationDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::Invalid(error) => Some(error),
            Self::Shape => None,
        }
    }
}

impl From<NotificationDecodeError> for crate::storage::failure::BackendCause {
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
            _ => Self::Emit,
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

/// What a notification message carries besides its attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PayloadFormat {
    /// The object's JSON API resource, which the API defaults to.
    #[default]
    JsonApiV1,
    None,
}

impl PayloadFormat {
    const ALL: [Self; 2] = [Self::JsonApiV1, Self::None];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::JsonApiV1 => "JSON_API_V1",
            Self::None => "NONE",
        }
    }
}

/// Not a `string_enum!`, whose error names no input: a rejected format names the one given.
impl std::str::FromStr for PayloadFormat {
    type Err = NotificationError;

    fn from_str(format: &str) -> Result<Self, NotificationError> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == format)
            .ok_or_else(|| NotificationError::InvalidPayloadFormat(format.to_owned()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotificationConfig {
    pub topic: Topic,
    pub payload_format: PayloadFormat,
    pub event_types: EventTypes,
    pub custom_attributes: BTreeMap<String, String>,
    /// Empty means every object name.
    pub object_name_prefix: String,
}

/// A rejected notification configuration. `Display` is the client-visible JSON error message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotificationError {
    InvalidTopic(String),
    InvalidPayloadFormat(String),
    InvalidEventType(String),
    DuplicateEventType(Event),
    TooManyCustomAttributes(usize),
    InvalidCustomAttributeKey(String),
    CustomAttributeTooLong(String),
    ObjectNamePrefixTooLong,
    BucketLimit,
    EventTypeLimit(Event),
}

impl fmt::Display for NotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTopic(topic) => write!(
                f,
                "Invalid topic {topic:?}; use //pubsub.googleapis.com/projects/PROJECT/topics/TOPIC (for example, //pubsub.googleapis.com/projects/local-project/topics/uploads)"
            ),
            Self::InvalidPayloadFormat(format) => write!(
                f,
                "Invalid payload_format {format:?}; use JSON_API_V1 or NONE"
            ),
            Self::InvalidEventType(event) => write!(
                f,
                "Invalid event type {event:?}; use OBJECT_FINALIZE, OBJECT_METADATA_UPDATE, OBJECT_DELETE or OBJECT_ARCHIVE"
            ),
            Self::DuplicateEventType(event) => {
                write!(f, "Duplicate event type {event}; list each event type once")
            }
            Self::TooManyCustomAttributes(count) => write!(
                f,
                "custom_attributes has {count} entries; a notification allows at most {MAX_CUSTOM_ATTRIBUTES}"
            ),
            Self::InvalidCustomAttributeKey(key) => write!(
                f,
                "Invalid custom attribute key {key:?}; use 1 to 256 bytes that do not start with goog"
            ),
            Self::CustomAttributeTooLong(key) => {
                write!(f, "Custom attribute {key:?} exceeds 1024 bytes")
            }
            Self::ObjectNamePrefixTooLong => f.write_str("object_name_prefix exceeds 1024 bytes"),
            Self::BucketLimit => write!(
                f,
                "Bucket already has {MAX_PER_BUCKET} notification configurations; delete one before adding another"
            ),
            Self::EventTypeLimit(event) => write!(
                f,
                "Bucket already has {MAX_PER_EVENT_TYPE} notification configurations for {event}; delete one or narrow event_types"
            ),
        }
    }
}

impl std::error::Error for NotificationError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotificationRecord {
    pub id: i64,
    pub config: NotificationConfig,
}

impl NotificationConfig {
    pub(crate) fn to_stored(&self) -> String {
        let custom_attributes = self
            .custom_attributes
            .iter()
            .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
            .collect::<Map<_, _>>();
        // serde_json's preserve_order keeps this member order, which stored rows have always had.
        Value::Object(Map::from_iter([
            ("topic".into(), Value::from(self.topic.as_str())),
            (
                "payload_format".into(),
                Value::from(self.payload_format.as_str()),
            ),
            ("event_types".into(), Value::from(&self.event_types)),
            ("custom_attributes".into(), Value::Object(custom_attributes)),
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
