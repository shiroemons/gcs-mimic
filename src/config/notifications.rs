use super::{
    Config, ConfigurationReport, Diagnostic, DiagnosticSource, Setting, SettingFix, Settings,
};
use crate::notification::{self, Endpoint, GlobalConfig, NotificationConfig, Runtime};
use std::{collections::BTreeMap, io};

pub(super) fn apply(config: &mut Config, setting: Setting, value: &str) -> Result<(), SettingFix> {
    match setting {
        Setting::PubsubEmulatorHost => {
            if !value.is_empty() {
                Endpoint::parse(value)
                    .map_err(|_| SettingFix::from(notification::publisher::INVALID_ENDPOINT))?;
            }
            config.pubsub_emulator_host = (!value.is_empty()).then(|| value.to_owned());
        }
        Setting::EventPubsubProjectId => value.clone_into(&mut config.event_pubsub_project_id),
        Setting::EventPubsubTopic => value.clone_into(&mut config.event_pubsub_topic),
        Setting::EventBucket => value.clone_into(&mut config.event_bucket),
        Setting::EventObjectPrefix => value.clone_into(&mut config.event_object_prefix),
        Setting::EventList => value.clone_into(&mut config.event_list),
        _ => return Err(super::unknown_setting()),
    }
    Ok(())
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

impl ConfigurationReport {
    pub(super) fn check_notifications(&mut self, settings: &Settings) {
        let keys = [
            "pubsub_emulator_host",
            "event_pubsub_project_id",
            "event_pubsub_topic",
            "event_bucket",
            "event_object_prefix",
            "event_list",
        ];
        if self.any_invalid(&keys) {
            return;
        }
        if let Err(error) = self.config.notification_runtime() {
            let input = keys.iter().find_map(|key| settings.get(*key));
            let (source, key) = input
                .map_or((DiagnosticSource::Config, "event_pubsub_topic"), |input| {
                    (input.source, input.key.as_str())
                });
            self.diagnostics
                .errors
                .push(Diagnostic::new(source, key, error.to_string()));
        }
    }
}

#[cfg(test)]
mod tests;
