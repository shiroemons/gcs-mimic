//! Bucket Pub/Sub notification configurations shared by the JSON adapter and the catalog.
//!
//! gcs-mimic stores, validates and reads back configurations but never publishes events:
//! the emulator does not contact Google Cloud. The supported subset is recorded in
//! `docs/en/slices/bucket-notifications.md`.

use std::collections::BTreeMap;

/// Event types listed in the Cloud Storage Pub/Sub notification documentation.
pub const EVENT_TYPES: [&str; 4] = [
    "OBJECT_FINALIZE",
    "OBJECT_METADATA_UPDATE",
    "OBJECT_DELETE",
    "OBJECT_ARCHIVE",
];
pub const PAYLOAD_FORMATS: [&str; 2] = ["JSON_API_V1", "NONE"];
/// Documented per-bucket total, per-event-type and custom-attribute limits.
pub const MAX_PER_BUCKET: usize = 100;
pub const MAX_PER_EVENT_TYPE: usize = 10;
pub const MAX_CUSTOM_ATTRIBUTES: usize = 10;
const TOPIC_PREFIX: &str = "//pubsub.googleapis.com/";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationConfig {
    /// Canonical `//pubsub.googleapis.com/projects/{project}/topics/{topic}` form.
    pub topic: String,
    pub payload_format: String,
    /// Empty means every event type, matching the documented default.
    pub event_types: Vec<String>,
    pub custom_attributes: BTreeMap<String, String>,
    /// Empty means every object name.
    pub object_name_prefix: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRecord {
    pub id: i64,
    pub config: NotificationConfig,
}

impl NotificationConfig {
    /// Event types this configuration triggers on, used for the per-event-type limit.
    pub fn effective_event_types(&self) -> impl Iterator<Item = &str> {
        let all = self.event_types.is_empty();
        EVENT_TYPES
            .into_iter()
            .filter(move |event| all || self.event_types.iter().any(|value| value == event))
    }

    pub fn to_stored(&self) -> String {
        serde_json::json!({
            "topic": self.topic,
            "payload_format": self.payload_format,
            "event_types": self.event_types,
            "custom_attributes": self.custom_attributes,
            "object_name_prefix": self.object_name_prefix,
        })
        .to_string()
    }

    /// Re-validate stored rows so a corrupt catalog cannot render configurations the API rejects.
    pub fn from_stored(stored: &str) -> Result<Self, &'static str> {
        let value: serde_json::Value =
            serde_json::from_str(stored).map_err(|_| "invalid stored notification")?;
        let string = |key: &str| {
            value[key]
                .as_str()
                .map(str::to_owned)
                .ok_or("invalid stored notification")
        };
        let event_types = value["event_types"]
            .as_array()
            .ok_or("invalid stored notification")?
            .iter()
            .map(|event| {
                event
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("invalid stored notification")
            })
            .collect::<Result<Vec<_>, _>>()?;
        let custom_attributes = value["custom_attributes"]
            .as_object()
            .ok_or("invalid stored notification")?
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_owned()))
                    .ok_or("invalid stored notification")
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let config = Self {
            topic: string("topic")?,
            payload_format: string("payload_format")?,
            event_types,
            custom_attributes,
            object_name_prefix: string("object_name_prefix")?,
        };
        let canonical =
            normalize_topic(&config.topic).map_err(|_| "invalid stored notification")?;
        if canonical != config.topic
            || validate_payload_format(&config.payload_format).is_err()
            || validate_event_types(&config.event_types).is_err()
            || validate_custom_attributes(&config.custom_attributes).is_err()
            || validate_object_name_prefix(&config.object_name_prefix).is_err()
        {
            return Err("invalid stored notification");
        }
        Ok(config)
    }
}

/// Accept the documented full resource name and the relative `projects/.../topics/...` name
/// that official SDKs send, and store only the full form.
pub fn normalize_topic(topic: &str) -> Result<String, String> {
    let relative = topic.strip_prefix(TOPIC_PREFIX).unwrap_or(topic);
    let invalid = || {
        format!(
            "Invalid topic {topic:?}; use //pubsub.googleapis.com/projects/PROJECT/topics/TOPIC (for example, //pubsub.googleapis.com/projects/local-project/topics/uploads)"
        )
    };
    let mut parts = relative.split('/');
    let (Some("projects"), Some(project), Some("topics"), Some(name), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(invalid());
    };
    // Project IDs may be domain-scoped (example.com:project); reject only unusable path text.
    if project.is_empty()
        || project.len() > 256
        || !project
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-.:_".contains(&byte))
    {
        return Err(invalid());
    }
    // Pub/Sub resource-name rules for topic IDs.
    if !(3..=255).contains(&name.len())
        || !name.as_bytes()[0].is_ascii_alphabetic()
        || name.starts_with("goog")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.~+%".contains(&byte))
    {
        return Err(invalid());
    }
    Ok(format!("{TOPIC_PREFIX}projects/{project}/topics/{name}"))
}

pub fn validate_payload_format(format: &str) -> Result<(), String> {
    if PAYLOAD_FORMATS.contains(&format) {
        Ok(())
    } else {
        Err(format!(
            "Invalid payload_format {format:?}; use JSON_API_V1 or NONE"
        ))
    }
}

pub fn validate_event_types(events: &[String]) -> Result<(), String> {
    for (index, event) in events.iter().enumerate() {
        if !EVENT_TYPES.contains(&event.as_str()) {
            return Err(format!(
                "Invalid event type {event:?}; use OBJECT_FINALIZE, OBJECT_METADATA_UPDATE, OBJECT_DELETE or OBJECT_ARCHIVE"
            ));
        }
        if events[..index].contains(event) {
            return Err(format!(
                "Duplicate event type {event}; list each event type once"
            ));
        }
    }
    Ok(())
}

pub fn validate_custom_attributes(attributes: &BTreeMap<String, String>) -> Result<(), String> {
    if attributes.len() > MAX_CUSTOM_ATTRIBUTES {
        return Err(format!(
            "custom_attributes has {} entries; a notification allows at most {MAX_CUSTOM_ATTRIBUTES}",
            attributes.len()
        ));
    }
    // Pub/Sub message attribute bounds: these values are copied onto every published message.
    for (key, value) in attributes {
        if key.is_empty() || key.len() > 256 || key.starts_with("goog") {
            return Err(format!(
                "Invalid custom attribute key {key:?}; use 1 to 256 bytes that do not start with goog"
            ));
        }
        if value.len() > 1024 {
            return Err(format!("Custom attribute {key:?} exceeds 1024 bytes"));
        }
    }
    Ok(())
}

pub fn validate_object_name_prefix(prefix: &str) -> Result<(), String> {
    // A prefix longer than the object-name limit could never match an object.
    if prefix.len() > 1024 {
        return Err("object_name_prefix exceeds 1024 bytes".into());
    }
    Ok(())
}

/// Check the documented per-bucket and per-event-type limits before adding `candidate`.
pub fn check_limits(
    existing: &[NotificationConfig],
    candidate: &NotificationConfig,
) -> Result<(), String> {
    if existing.len() >= MAX_PER_BUCKET {
        return Err(format!(
            "Bucket already has {MAX_PER_BUCKET} notification configurations; delete one before adding another"
        ));
    }
    for event in candidate.effective_event_types() {
        let count = existing
            .iter()
            .filter(|config| config.effective_event_types().any(|other| other == event))
            .count();
        if count >= MAX_PER_EVENT_TYPE {
            return Err(format!(
                "Bucket already has {MAX_PER_EVENT_TYPE} notification configurations for {event}; delete one or narrow event_types"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(events: &[&str]) -> NotificationConfig {
        NotificationConfig {
            topic: "//pubsub.googleapis.com/projects/p/topics/topic".into(),
            payload_format: "JSON_API_V1".into(),
            event_types: events.iter().map(|event| (*event).into()).collect(),
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
                normalize_topic(topic).unwrap(),
                "//pubsub.googleapis.com/projects/local-project/topics/uploads"
            );
        }
        assert_eq!(
            normalize_topic("projects/example.com:p/topics/a.b~c+d%e_f-g").unwrap(),
            "//pubsub.googleapis.com/projects/example.com:p/topics/a.b~c+d%e_f-g"
        );
        let longest = format!("projects/p/topics/t{}", "a".repeat(254));
        assert!(normalize_topic(&longest).is_ok());
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
            assert!(normalize_topic(topic).is_err(), "{topic}");
        }
    }

    #[test]
    fn fields_reject_unknown_duplicate_and_oversized_values() {
        assert!(validate_payload_format("JSON_API_V1").is_ok());
        assert!(validate_payload_format("NONE").is_ok());
        assert!(validate_payload_format("json_api_v1").is_err());
        assert!(validate_event_types(&["OBJECT_FINALIZE".into(), "OBJECT_ARCHIVE".into()]).is_ok());
        assert!(validate_event_types(&["OBJECT_CREATE".into()]).is_err());
        assert!(validate_event_types(&["OBJECT_DELETE".into(), "OBJECT_DELETE".into()]).is_err());
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
        let full: Vec<_> = EVENT_TYPES
            .iter()
            .flat_map(|event| vec![config(&[event]); 10])
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
}
