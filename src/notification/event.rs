//! Pub/Sub payload encoding reuses the JSON object's supported metadata representation.

use super::{NotificationConfig, NotificationError, PayloadFormat};
use crate::{bucket::CatalogError, object::ObjectRecord};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, time::SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Finalize,
    MetadataUpdate,
    Delete,
    Archive,
}

impl Event {
    /// In the order the Cloud Storage documentation lists them, which the per-event-type limit
    /// checks and reports in.
    pub(crate) const ALL: [Self; 4] = [
        Self::Finalize,
        Self::MetadataUpdate,
        Self::Delete,
        Self::Archive,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Finalize => "OBJECT_FINALIZE",
            Self::MetadataUpdate => "OBJECT_METADATA_UPDATE",
            Self::Delete => "OBJECT_DELETE",
            Self::Archive => "OBJECT_ARCHIVE",
        }
    }
}

impl std::str::FromStr for Event {
    type Err = NotificationError;

    fn from_str(name: &str) -> Result<Self, NotificationError> {
        Self::ALL
            .into_iter()
            .find(|event| event.as_str() == name)
            .ok_or_else(|| NotificationError::InvalidEventType(name.to_owned()))
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The event types a configuration names, in the order named and each once. Empty selects every
/// type, as the API documents; the order is what the API echoes back and the catalog stores.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EventTypes(Vec<Event>);

impl EventTypes {
    /// Parses names in order and rejects the first unknown or repeated one.
    pub(crate) fn parse<'a>(
        names: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, NotificationError> {
        let mut events = Self::default();
        for name in names {
            let event: Event = name.parse()?;
            if !events.insert(event) {
                return Err(NotificationError::DuplicateEventType(event));
            }
        }
        Ok(events)
    }

    /// Adds `event` unless it is named already; whether it was added.
    pub(crate) fn insert(&mut self, event: Event) -> bool {
        let added = !self.0.contains(&event);
        if added {
            self.0.push(event);
        }
        added
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The named types, in order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = Event> + '_ {
        self.0.iter().copied()
    }

    pub(crate) fn selects(&self, event: Event) -> bool {
        self.0.is_empty() || self.0.contains(&event)
    }

    /// Every type this selects, in [`Event::ALL`] order.
    pub(crate) fn effective(&self) -> impl Iterator<Item = Event> + '_ {
        Event::ALL.into_iter().filter(|event| self.selects(*event))
    }
}

/// Keeps the first occurrence of each type, as the `event_list` setting always has.
impl FromIterator<Event> for EventTypes {
    fn from_iter<I: IntoIterator<Item = Event>>(events: I) -> Self {
        let mut types = Self::default();
        for event in events {
            types.insert(event);
        }
        types
    }
}

/// The array the API renders and the catalog stores.
impl From<&EventTypes> for Value {
    fn from(events: &EventTypes) -> Self {
        Self::Array(
            events
                .iter()
                .map(|event| Self::from(event.as_str()))
                .collect(),
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Message<'a> {
    pub(crate) event: Event,
    pub(crate) object: &'a ObjectRecord,
    pub(crate) time: SystemTime,
    pub(crate) replacement: Option<i64>,
}

impl Message<'_> {
    pub(crate) fn matches(&self, config: &NotificationConfig) -> bool {
        self.object.name.starts_with(&config.object_name_prefix)
            && config.event_types.selects(self.event)
    }

    pub(crate) fn encode(
        &self,
        config: &NotificationConfig,
        config_id: Option<i64>,
    ) -> Result<String, CatalogError> {
        let invalid_time = |error| {
            CatalogError::Corrupt(crate::storage::failure::BackendCause::with_source(
                "invalid notification timestamp",
                error,
            ))
        };
        let mut attributes: BTreeMap<String, String> = config.custom_attributes.clone();
        for (key, value) in [
            ("eventType", self.event.as_str().to_owned()),
            ("bucketId", self.object.bucket.clone()),
            ("objectId", self.object.name.clone()),
            ("objectGeneration", self.object.generation.to_string()),
            ("payloadFormat", config.payload_format.as_str().to_owned()),
            (
                "eventTime",
                crate::json::timestamp(self.time).map_err(invalid_time)?,
            ),
        ] {
            attributes.insert(key.into(), value);
        }
        if let Some(id) = config_id {
            attributes.insert(
                "notificationConfig".into(),
                format!(
                    "projects/_/buckets/{}/notificationConfigs/{id}",
                    self.object.bucket
                ),
            );
        }
        if let Some(generation) = self.replacement {
            let key = if self.event == Event::Finalize {
                "overwroteGeneration"
            } else {
                "overwrittenByGeneration"
            };
            attributes.insert(key.into(), generation.to_string());
        }
        let mut message = serde_json::Map::from_iter([("attributes".into(), json!(attributes))]);
        match config.payload_format {
            PayloadFormat::JsonApiV1 => {
                let mut payload =
                    crate::json::object_resource(self.object, "", crate::json::Projection::NoAcl)
                        .map_err(invalid_time)?;
                // Notification metadata has no listener-dependent URLs.
                if let Some(fields) = payload.as_object_mut() {
                    // shift_remove keeps the recorded order of the remaining fields.
                    fields.shift_remove("selfLink");
                    fields.shift_remove("mediaLink");
                    if matches!(self.event, Event::Delete | Event::Archive) {
                        fields.insert(
                            "timeDeleted".into(),
                            Value::String(crate::json::timestamp(self.time).map_err(invalid_time)?),
                        );
                    }
                }
                message.insert(
                    "data".into(),
                    Value::String(crate::json::base64(payload.to_string().as_bytes())),
                );
            }
            PayloadFormat::None => {}
        }
        Ok(json!({"messages": [message]}).to_string())
    }
}
