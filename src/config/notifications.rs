use super::{ConfigDraft, Diagnostic, DiagnosticSource, Setting, SettingFix, Settings, Validation};
use crate::bucket::name::MAX_BUCKET_NAME_BYTES;
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
    /// The project and topic do not name a valid topic.
    Topic(NotificationError),
    /// The object name prefix filter is not one a notification configuration accepts.
    ObjectPrefix(NotificationError),
    BucketTooLong,
    EventList,
}

impl EventSettingsError {
    /// The settings a diagnostic names, most specific first; it names the first one given.
    fn settings(&self) -> &'static [Setting] {
        match self {
            Self::Endpoint(_) => &[Setting::PubsubEmulatorHost],
            Self::FiltersWithoutTopic => &[
                Setting::EventBucket,
                Setting::EventObjectPrefix,
                Setting::EventList,
            ],
            Self::Incomplete => &[
                Setting::EventPubsubProjectId,
                Setting::EventPubsubTopic,
                Setting::PubsubEmulatorHost,
            ],
            Self::Topic(_) => &[Setting::EventPubsubTopic, Setting::EventPubsubProjectId],
            Self::ObjectPrefix(_) => &[Setting::EventObjectPrefix],
            Self::BucketTooLong => &[Setting::EventBucket],
            Self::EventList => &[Setting::EventList],
        }
    }
}

// The endpoint and notification errors are both displayed and exposed as concrete sources.
// Keep those identities until the diagnostic contract is revisited.
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
            Self::Topic(error) | Self::ObjectPrefix(error) => error.fmt(f),
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
            Self::Topic(error) | Self::ObjectPrefix(error) => Some(error),
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
                .iter()
                .copied()
                .find(|event| list_name(*event) == name)
                .ok_or(EventSettingsError::EventList)
        })
        .collect::<Result<EventTypes, _>>()
        .map(Some)
}

impl ConfigDraft {
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
        .map_err(EventSettingsError::Topic)?;
        notification::validate_object_name_prefix(&self.event_object_prefix)
            .map_err(EventSettingsError::ObjectPrefix)?;
        if self.event_bucket.len() > MAX_BUCKET_NAME_BYTES {
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

/// The settings the event publishing runtime reads. A diagnostic falls back to the first given in
/// this order when none of its own settings was given.
const EVENT_SETTINGS: [Setting; 6] = [
    Setting::PubsubEmulatorHost,
    Setting::EventPubsubProjectId,
    Setting::EventPubsubTopic,
    Setting::EventBucket,
    Setting::EventObjectPrefix,
    Setting::EventList,
];

impl Validation {
    pub(super) fn check_notifications(&mut self, settings: &Settings) -> Runtime {
        if self.any_invalid(&EVENT_SETTINGS) {
            return Runtime::default();
        }
        let error = match self.draft.notification_runtime() {
            Ok(runtime) => return runtime,
            Err(error) => error,
        };
        let input = error
            .settings()
            .iter()
            .chain(&EVENT_SETTINGS)
            .find_map(|setting| settings.get(setting));
        let (source, key) = input.map_or(
            (DiagnosticSource::Config, Setting::EventPubsubTopic.as_str()),
            |input| (input.source, input.key.as_str()),
        );
        self.diagnostics
            .errors
            .push(Diagnostic::new(source, key, error.to_string()));
        Runtime::default()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    for error in [
        EventSettingsError::Endpoint(InvalidEndpoint),
        EventSettingsError::Topic(NotificationError::BucketLimit),
        EventSettingsError::ObjectPrefix(NotificationError::ObjectNamePrefixTooLong),
    ] {
        let source = std::error::Error::source(&error).unwrap();
        assert!(source.is::<InvalidEndpoint>() || source.is::<NotificationError>());
        assert_eq!(error.to_string(), source.to_string());
    }
}
