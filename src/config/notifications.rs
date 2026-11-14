use super::{
    Config, ConfigurationReport, Diagnostic, DiagnosticSource, Setting, SettingFix, Settings,
};
use crate::notification::{
    self, Endpoint, Event, EventTypes, GlobalConfig, NotificationConfig, NotificationError,
    PayloadFormat, Runtime, Topic, publisher::InvalidEndpoint,
};
use std::{collections::BTreeMap, fmt};

/// The legacy `event_list` default; any other text counts as a filter. The comparison is exact,
/// as fake-gcs-server's flag default is this word.
pub(super) const DEFAULT_EVENT_LIST: &str = "finalize";

/// An empty value disables delivery; anything else must name a local emulator.
pub(super) fn emulator_host(value: &str) -> Result<Option<String>, SettingFix> {
    if value.is_empty() {
        return Ok(None);
    }
    Endpoint::parse(value)?;
    Ok(Some(value.to_owned()))
}

/// Why the event settings select no usable global notification rule. `Display` is the
/// configuration diagnostic.
#[derive(Debug)]
pub(crate) enum EventSettingsError {
    Endpoint(InvalidEndpoint),
    /// Filters without the project and topic they would filter for.
    FiltersWithoutTopic,
    /// Publishing needs the project, the topic and an explicit local emulator.
    Incomplete,
    Invalid(NotificationError),
    BucketTooLong,
    EventList,
}

impl fmt::Display for EventSettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Endpoint(error) => error.fmt(f),
            Self::FiltersWithoutTopic => f.write_str(
                "Event filters require event_pubsub_project_id and event_pubsub_topic; remove the filters or configure a local Pub/Sub emulator",
            ),
            Self::Incomplete => f.write_str(
                "Event publishing requires event_pubsub_project_id, event_pubsub_topic and an explicit pubsub_emulator_host (or PUBSUB_EMULATOR_HOST); production Google Pub/Sub is disabled",
            ),
            Self::Invalid(error) => error.fmt(f),
            Self::BucketTooLong => {
                f.write_str("event_bucket exceeds the supported bucket name length")
            }
            Self::EventList => f.write_str(
                "event_list requires finalize, delete, metadataUpdate or archive, separated by commas",
            ),
        }
    }
}

impl std::error::Error for EventSettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Endpoint(error) => Some(error),
            Self::Invalid(error) => Some(error),
            Self::FiltersWithoutTopic
            | Self::Incomplete
            | Self::BucketTooLong
            | Self::EventList => None,
        }
    }
}

/// `event_list` spells event types as fake-gcs-server's `-event.list` does. This is the
/// setting's spelling, not the API's, so it lives with the setting.
fn list_name(event: Event) -> &'static str {
    match event {
        Event::Finalize => "finalize",
        Event::MetadataUpdate => "metadataUpdate",
        Event::Delete => "delete",
        Event::Archive => "archive",
    }
}

/// `None` for `none`, which disables the global rule; repeats are dropped, not rejected.
fn event_list(list: &str) -> Result<Option<EventTypes>, EventSettingsError> {
    if list.trim() == "none" {
        return Ok(None);
    }
    list.split(',')
        .map(str::trim)
        .map(|name| {
            Event::ALL
                .into_iter()
                .find(|event| list_name(*event) == name)
                .ok_or(EventSettingsError::EventList)
        })
        .collect::<Result<EventTypes, _>>()
        .map(Some)
}

impl Config {
    pub(crate) fn notification_runtime(&self) -> Result<Runtime, EventSettingsError> {
        let endpoint = self
            .pubsub_emulator_host
            .as_deref()
            .map(Endpoint::parse)
            .transpose()
            .map_err(EventSettingsError::Endpoint)?;
        let enabled =
            !self.event_pubsub_project_id.is_empty() || !self.event_pubsub_topic.is_empty();
        if !enabled {
            if !self.event_bucket.is_empty()
                || !self.event_object_prefix.is_empty()
                || self.event_list != DEFAULT_EVENT_LIST
            {
                return Err(EventSettingsError::FiltersWithoutTopic);
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
            return Err(EventSettingsError::Incomplete);
        }
        let topic = format!(
            "projects/{}/topics/{}",
            self.event_pubsub_project_id, self.event_pubsub_topic
        )
        .parse::<Topic>()
        .map_err(EventSettingsError::Invalid)?;
        notification::validate_object_name_prefix(&self.event_object_prefix)
            .map_err(EventSettingsError::Invalid)?;
        if self.event_bucket.len() > 222 {
            return Err(EventSettingsError::BucketTooLong);
        }
        let Some(event_types) = event_list(&self.event_list)? else {
            return Ok(Runtime {
                endpoint,
                global: None,
            });
        };
        Ok(Runtime {
            endpoint,
            global: Some(GlobalConfig {
                bucket: self.event_bucket.clone(),
                notification: NotificationConfig {
                    topic,
                    payload_format: PayloadFormat::JsonApiV1,
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
