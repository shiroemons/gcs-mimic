//! Pub/Sub payload encoding reuses the JSON object's supported metadata representation.

use super::NotificationConfig;
use crate::{bucket::CatalogError, object::ObjectRecord};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Finalize,
    MetadataUpdate,
    Delete,
    Archive,
}

impl Event {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Finalize => "OBJECT_FINALIZE",
            Self::MetadataUpdate => "OBJECT_METADATA_UPDATE",
            Self::Delete => "OBJECT_DELETE",
            Self::Archive => "OBJECT_ARCHIVE",
        }
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
            && config
                .effective_event_types()
                .any(|event| event == self.event.as_str())
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
            ("payloadFormat", config.payload_format.clone()),
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
        let mut message = json!({"attributes": attributes});
        if config.payload_format == "JSON_API_V1" {
            let mut payload =
                crate::json::object_resource(self.object, "", crate::json::Projection::NoAcl)
                    .map_err(invalid_time)?;
            // Notification metadata has no listener-dependent URLs.
            if let Some(fields) = payload.as_object_mut() {
                fields.remove("selfLink");
                fields.remove("mediaLink");
            }
            if matches!(self.event, Event::Delete | Event::Archive) {
                payload["timeDeleted"] =
                    Value::String(crate::json::timestamp(self.time).map_err(invalid_time)?);
            }
            message["data"] = Value::String(crate::json::base64(payload.to_string().as_bytes()));
        }
        Ok(json!({"messages": [message]}).to_string())
    }
}
