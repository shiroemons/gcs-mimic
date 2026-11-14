use super::{
    Config, ConfigurationReport, Diagnostic, DiagnosticSource, Setting, SettingFix, Settings,
};
use crate::notification::{self, Endpoint, GlobalConfig, NotificationConfig, Runtime};
use std::{collections::BTreeMap, io};

/// An empty value disables delivery; anything else must name a local emulator.
pub(super) fn emulator_host(value: &str) -> Result<Option<String>, SettingFix> {
    if value.is_empty() {
        return Ok(None);
    }
    Endpoint::parse(value)
        .map_err(|_| SettingFix::from(notification::publisher::INVALID_ENDPOINT))?;
    Ok(Some(value.to_owned()))
}

impl Config {
    pub(crate) fn notification_runtime(&self) -> io::Result<Runtime> {
        let invalid = |message| io::Error::new(io::ErrorKind::InvalidInput, message);
        let endpoint = self
            .pubsub_emulator_host
            .as_deref()
            .map(Endpoint::parse)
            .transpose()?;
        let enabled =
            !self.event_pubsub_project_id.is_empty() || !self.event_pubsub_topic.is_empty();
        if !enabled {
            if !self.event_bucket.is_empty()
                || !self.event_object_prefix.is_empty()
                || self.event_list != "finalize"
            {
                return Err(invalid(
                    "Event filters require event_pubsub_project_id and event_pubsub_topic; remove the filters or configure a local Pub/Sub emulator",
                ));
            }
            return Ok(Runtime {
                endpoint,
                global: None,
            });
        }
        if self.event_pubsub_project_id.is_empty()
            || self.event_pubsub_topic.is_empty()
            || endpoint.is_none()
        {
            return Err(invalid(
                "Event publishing requires event_pubsub_project_id, event_pubsub_topic and an explicit pubsub_emulator_host (or PUBSUB_EMULATOR_HOST); production Google Pub/Sub is disabled",
            ));
        }
        let topic = notification::normalize_topic(&format!(
            "projects/{}/topics/{}",
            self.event_pubsub_project_id, self.event_pubsub_topic
        ))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        notification::validate_object_name_prefix(&self.event_object_prefix)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        if self.event_bucket.len() > 222 {
            return Err(invalid(
                "event_bucket exceeds the supported bucket name length",
            ));
        }
        if self.event_list.trim() == "none" {
            return Ok(Runtime {
                endpoint,
                global: None,
            });
        }
        let mut event_types = vec![];
        for event in self.event_list.split(',').map(str::trim) {
            let event = match event {
                "finalize" => "OBJECT_FINALIZE",
                "delete" => "OBJECT_DELETE",
                "metadataUpdate" => "OBJECT_METADATA_UPDATE",
                "archive" => "OBJECT_ARCHIVE",
                _ => {
                    return Err(invalid(
                        "event_list requires finalize, delete, metadataUpdate or archive, separated by commas",
                    ));
                }
            };
            if !event_types.iter().any(|existing| existing == event) {
                event_types.push(event.to_owned());
            }
        }
        Ok(Runtime {
            endpoint,
            global: Some(GlobalConfig {
                bucket: self.event_bucket.clone(),
                notification: NotificationConfig {
                    topic,
                    payload_format: "JSON_API_V1".into(),
                    event_types,
                    custom_attributes: BTreeMap::new(),
                    object_name_prefix: self.event_object_prefix.clone(),
                },
            }),
        })
    }
}

/// The settings the event publishing runtime reads, in the order a diagnostic looks for the one
/// to name.
const EVENT_SETTINGS: [Setting; 6] = [
    Setting::PubsubEmulatorHost,
    Setting::EventPubsubProjectId,
    Setting::EventPubsubTopic,
    Setting::EventBucket,
    Setting::EventObjectPrefix,
    Setting::EventList,
];

impl ConfigurationReport {
    pub(super) fn check_notifications(&mut self, settings: &Settings) {
        if self.any_invalid(&EVENT_SETTINGS) {
            return;
        }
        if let Err(error) = self.config.notification_runtime() {
            let input = EVENT_SETTINGS
                .iter()
                .find_map(|setting| settings.get(setting));
            let (source, key) = input.map_or(
                (DiagnosticSource::Config, Setting::EventPubsubTopic.as_str()),
                |input| (input.source, input.key.as_str()),
            );
            self.diagnostics
                .errors
                .push(Diagnostic::new(source, key, error.to_string()));
        }
    }
}

#[cfg(test)]
mod tests;
