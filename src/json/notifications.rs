//! JSON wire adapter for `storage.notifications.*` (`b/{bucket}/notificationConfigs`).
//!
//! Routing, request validation and rendering only; persistence and authorization live in the
//! catalog. Configurations are stored and read back, but no Pub/Sub message is ever published.

use super::{Fields, JsonError, buckets::encode_segment, query};
use crate::{
    config::CompatProfile,
    notification::{
        self, EventTypes, NotificationConfig, NotificationRecord, PayloadFormat, Topic,
    },
};
use hyper::{Method, StatusCode, Uri};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotificationRequest {
    Insert {
        bucket: String,
    },
    List {
        bucket: String,
    },
    Get {
        bucket: String,
        notification: String,
    },
    Delete {
        bucket: String,
        notification: String,
    },
}

impl NotificationRequest {
    pub(crate) fn bucket(&self) -> &str {
        match self {
            Self::Insert { bucket }
            | Self::List { bucket }
            | Self::Get { bucket, .. }
            | Self::Delete { bucket, .. } => bucket,
        }
    }
}

/// Match `b/{bucket}/notificationConfigs[/{notification}]` before any other bucket route.
pub(crate) fn route(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
) -> Option<Result<(NotificationRequest, Fields), JsonError>> {
    let rest = uri.path().strip_prefix("/storage/v1/b/")?;
    let (segment, rest) = rest.split_once('/')?;
    let notification = match rest.strip_prefix("notificationConfigs")? {
        "" => None,
        item => Some(item.strip_prefix('/')?),
    };
    if segment.is_empty() || notification.is_some_and(|item| item.is_empty() || item.contains('/'))
    {
        return None;
    }
    Some((|| {
        if segment.len() > 222 * 3 {
            return Err(JsonError::invalid_parameter("Invalid bucket name"));
        }
        let bucket = query::decode(segment, query::Decoding::Path)?;
        if profile == CompatProfile::Native {
            super::buckets::validate_bucket_name_for_profile(&bucket, profile)?;
        }
        let allowed = if notification.is_some() {
            "GET, DELETE"
        } else {
            "GET, POST"
        };
        let request = match (method, notification) {
            (&Method::POST, None) => NotificationRequest::Insert { bucket },
            (&Method::GET, None) => NotificationRequest::List { bucket },
            (&Method::GET, Some(item)) => NotificationRequest::Get {
                bucket,
                notification: query::decode(item, query::Decoding::Path)?,
            },
            (&Method::DELETE, Some(item)) => NotificationRequest::Delete {
                bucket,
                notification: query::decode(item, query::Decoding::Path)?,
            },
            _ => {
                return Err(JsonError::method_not_allowed(
                    format!(
                        "HTTP method is not supported for this notification endpoint; use {allowed}"
                    ),
                    Some(allowed),
                ));
            }
        };
        let mut query = query::parse_query(uri.query().unwrap_or_default())?;
        let schema = if matches!(request, NotificationRequest::List { .. }) {
            "Notifications"
        } else {
            "Notification"
        };
        let fields = Fields::take(&mut query, schema)?;
        query::common(&mut query)?;
        query::finish(&query)?;
        Ok((request, fields))
    })())
}

fn invalid(message: impl Into<String>) -> JsonError {
    JsonError::new(StatusCode::BAD_REQUEST, "invalid", message)
}

/// A rejected notification field becomes the error body text; the validation error has no
/// underlying cause to keep.
impl From<notification::NotificationError> for JsonError {
    fn from(error: notification::NotificationError) -> Self {
        // Cloud Storage's wording for the two commonest mistakes (recorded 2026-10-31); a bad
        // payload format is reported as a missing one.
        match error {
            notification::NotificationError::InvalidTopic(_) => invalid(
                "Invalid Google Cloud Pub/Sub topic. It should look like '//pubsub.googleapis.com/projects/*/topics/*.'",
            ),
            notification::NotificationError::InvalidPayloadFormat(_) => JsonError::required(
                "You must specify a payload format in the 'payload_format' field.",
            ),
            error => invalid(error.to_string()),
        }
    }
}

/// Parse a whole notifications.insert body without a topic existence check.
#[cfg(test)]
pub(crate) fn parse_notification(bytes: &[u8]) -> Result<NotificationConfig, JsonError> {
    parse_notification_head(bytes)?.finish()
}

/// A notification body whose topic and payload format are valid; the remaining fields are still
/// unchecked.
pub(crate) struct PendingNotification {
    body: Map<String, Value>,
    topic: Topic,
    payload_format: PayloadFormat,
}

impl PendingNotification {
    pub(crate) fn topic(&self) -> &Topic {
        &self.topic
    }

    /// Validate the fields that follow the topic existence check.
    pub(crate) fn finish(self) -> Result<NotificationConfig, JsonError> {
        parse_notification_tail(self)
    }
}

/// Parse a notifications.insert body up to and including the payload format. Output-only `id`,
/// `etag` and `selfLink` are ignored, following the server-assigned semantics of those Discovery
/// fields; other unknown fields follow the bucket insert ignored-input rule.
///
/// Cloud Storage validates in a fixed order (recorded 2026-10-31): the topic's presence and
/// form, the payload format, the topic's existence, then the event types. Parsing therefore stops
/// after the payload format, so the caller can check the topic before the rest is validated.
pub(crate) fn parse_notification_head(bytes: &[u8]) -> Result<PendingNotification, JsonError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| JsonError::parse_error(bytes, &error))?;
    let Value::Object(body) = value else {
        return Err(invalid("Notification resource must be a JSON object"));
    };
    let field = |name: &str| body.get(name).filter(|value| !value.is_null());
    let string = |name: &str| {
        field(name)
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| invalid(format!("{name} must be a string")))
            })
            .transpose()
    };
    if let Some(kind) = string("kind")?
        && kind != "storage#notification"
    {
        return Err(invalid("kind must be storage#notification"));
    }
    let topic: Topic = string("topic")?
        .ok_or_else(|| {
            JsonError::required("You must specify a Cloud Pub/Sub topic in the 'topic' field.")
        })?
        .parse()?;
    let payload_format = string("payload_format")?
        .map_or(Ok(PayloadFormat::JsonApiV1), str::parse::<PayloadFormat>)?;
    Ok(PendingNotification {
        body,
        topic,
        payload_format,
    })
}

fn parse_notification_tail(pending: PendingNotification) -> Result<NotificationConfig, JsonError> {
    let PendingNotification {
        body,
        topic,
        payload_format,
    } = pending;
    let field = |name: &str| body.get(name).filter(|value| !value.is_null());
    let string = |name: &str| {
        field(name)
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| invalid(format!("{name} must be a string")))
            })
            .transpose()
    };
    // Every element must be a string before any of them is checked as an event type.
    let event_names = match field("event_types") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| invalid("event_types must be an array of strings"))?
            .iter()
            .map(|event| {
                event
                    .as_str()
                    .ok_or_else(|| invalid("event_types must be an array of strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let event_types = EventTypes::parse(event_names)?;
    let custom_attributes = match field("custom_attributes") {
        None => BTreeMap::new(),
        Some(value) => value
            .as_object()
            .ok_or_else(|| invalid("custom_attributes must be an object of strings"))?
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| (key.clone(), value.to_owned()))
                    .ok_or_else(|| invalid("custom_attributes values must be strings"))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?,
    };
    notification::validate_custom_attributes(&custom_attributes)?;
    let object_name_prefix = string("object_name_prefix")?.unwrap_or_default().to_owned();
    notification::validate_object_name_prefix(&object_name_prefix)?;
    Ok(NotificationConfig {
        topic,
        payload_format,
        event_types,
        custom_attributes,
        object_name_prefix,
    })
}

/// Render a stored configuration. Configurations are immutable, so the ID doubles as the etag.
pub(crate) fn notification_resource(
    record: &NotificationRecord,
    bucket: &str,
    base_url: &str,
) -> Value {
    let id = record.id.to_string();
    let config = &record.config;
    let mut value = Map::from_iter([
        ("kind".into(), "storage#notification".into()),
        ("id".into(), json!(id)),
        (
            "selfLink".into(),
            json!(format!(
                "{}/storage/v1/b/{}/notificationConfigs/{id}",
                base_url.trim_end_matches('/'),
                encode_segment(bucket)
            )),
        ),
        ("topic".into(), Value::from(config.topic.as_str())),
        (
            "payload_format".into(),
            Value::from(config.payload_format.as_str()),
        ),
        ("etag".into(), json!(id)),
    ]);
    if !config.event_types.is_empty() {
        value.insert("event_types".into(), Value::from(&config.event_types));
    }
    if !config.custom_attributes.is_empty() {
        value.insert(
            "custom_attributes".into(),
            Value::Object(
                config
                    .custom_attributes
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
                    .collect::<Map<_, _>>(),
            ),
        );
    }
    if !config.object_name_prefix.is_empty() {
        value.insert(
            "object_name_prefix".into(),
            config.object_name_prefix.as_str().into(),
        );
    }
    Value::Object(value)
}

pub(crate) fn notification_list(
    records: &[NotificationRecord],
    bucket: &str,
    base_url: &str,
) -> Value {
    let mut value = Map::from_iter([("kind".into(), "storage#notifications".into())]);
    if !records.is_empty() {
        value.insert(
            "items".into(),
            records
                .iter()
                .map(|record| notification_resource(record, bucket, base_url))
                .collect(),
        );
    }
    Value::Object(value)
}

/// Cloud Storage's rejection of a topic the service agent cannot see (recorded 2026-10-31). The
/// recording only sent the full `//pubsub.googleapis.com/...` form, so naming the normalized
/// topic for a short `projects/P/topics/T` request is a guess. The
/// emulator has no permission model, so a missing topic is the only way to reach it.
pub(crate) fn topic_not_found(topic: &str, bucket_project: &str) -> JsonError {
    invalid(format!(
        "Cloud Pub/Sub topic '{topic}' not found, or user '{}' does not have permission to it.",
        super::projects::service_account_email(bucket_project)
    ))
}

/// The emulator could not say whether the topic exists. Accepting the configuration anyway would
/// hide a missing topic, so the request fails with a retryable error and creates nothing.
pub(crate) fn topic_unverifiable(error: &notification::TopicCheckError) -> JsonError {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "notification_topic_check_failed",
        json!({"cause": error_chain(error)}),
    );
    JsonError::service_unavailable(
        "Could not confirm that the Cloud Pub/Sub topic exists: the configured local Pub/Sub emulator is unreachable or gave an unexpected answer; start or repair the emulator (pubsub_emulator_host), then retry",
    )
}

/// The error and every `source()` below it, so an operator can tell a timeout from a refused
/// connection or a rejected endpoint. Only the log carries this; the client text is fixed.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

pub(crate) fn notification_not_found() -> JsonError {
    JsonError::new(
        StatusCode::NOT_FOUND,
        "notFound",
        "The requested resource was not found.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(path: &str) -> Uri {
        path.parse().unwrap()
    }

    fn routed(method: Method, path: &str) -> Option<Result<NotificationRequest, JsonError>> {
        route(&method, &uri(path), CompatProfile::Native).map(|result| result.map(|(r, _)| r))
    }

    #[test]
    fn routes_methods_paths_and_queries() {
        assert_eq!(
            routed(Method::POST, "/storage/v1/b/bucket/notificationConfigs"),
            Some(Ok(NotificationRequest::Insert {
                bucket: "bucket".into()
            }))
        );
        assert_eq!(
            routed(
                Method::GET,
                "/storage/v1/b/bucket/notificationConfigs?prettyPrint=false&alt=json&fields=items(id)"
            ),
            Some(Ok(NotificationRequest::List {
                bucket: "bucket".into()
            }))
        );
        assert_eq!(
            routed(
                Method::GET,
                "/storage/v1/b/bucket/notificationConfigs/7?fields=id,topic"
            ),
            Some(Ok(NotificationRequest::Get {
                bucket: "bucket".into(),
                notification: "7".into()
            }))
        );
        assert_eq!(
            routed(
                Method::DELETE,
                "/storage/v1/b/bucket/notificationConfigs/a%20b"
            ),
            Some(Ok(NotificationRequest::Delete {
                bucket: "bucket".into(),
                notification: "a b".into()
            }))
        );
        for path in [
            "/storage/v1/b/bucket/notificationConfigsX",
            "/storage/v1/b/bucket/notificationConfigs/",
            "/storage/v1/b/bucket/notificationConfigs/1/extra",
            "/storage/v1/b//notificationConfigs",
            "/storage/v1/b/bucket/o/notificationConfigs",
            "/storage/v1/b/bucket",
        ] {
            assert!(routed(Method::GET, path).is_none(), "{path}");
        }
        for (method, path, allow) in [
            (
                Method::PUT,
                "/storage/v1/b/bucket/notificationConfigs",
                "GET, POST",
            ),
            (
                Method::DELETE,
                "/storage/v1/b/bucket/notificationConfigs",
                "GET, POST",
            ),
            (
                Method::POST,
                "/storage/v1/b/bucket/notificationConfigs/1",
                "GET, DELETE",
            ),
            (
                Method::PATCH,
                "/storage/v1/b/bucket/notificationConfigs/1",
                "GET, DELETE",
            ),
        ] {
            let error = routed(method, path).unwrap().unwrap_err();
            assert_eq!(
                (error.status, error.allow),
                (StatusCode::METHOD_NOT_ALLOWED, Some(allow))
            );
        }
        for (path, status, reason) in [
            (
                "/storage/v1/b/Bad_Bucket/notificationConfigs",
                400,
                "invalidParameter",
            ),
            (
                "/storage/v1/b/bucket/notificationConfigs?unknown=1",
                400,
                "invalidParameter",
            ),
            (
                "/storage/v1/b/bucket/notificationConfigs?fields=bogus",
                400,
                "invalidParameter",
            ),
            (
                "/storage/v1/b/bucket/notificationConfigs?callback=p",
                400,
                "badRequest",
            ),
            (
                "/storage/v1/b/bucket/notificationConfigs?a=1&a=2",
                400,
                "invalidParameter",
            ),
        ] {
            let error = routed(Method::GET, path).unwrap().unwrap_err();
            assert_eq!(
                (error.status.as_u16(), error.reason),
                (status, reason),
                "{path}"
            );
        }
        // Cloud Storage served the billing project on these routes for a bucket without
        // Requester Pays as if it were absent (case bkt-edges-notification-ids).
        for path in [
            "/storage/v1/b/bucket/notificationConfigs?userProject=p",
            "/storage/v1/b/bucket/notificationConfigs/1?userProject=p",
        ] {
            assert!(routed(Method::GET, path).unwrap().is_ok(), "{path}");
        }
    }

    #[test]
    fn parses_defaults_normalizes_and_ignores_output_only_fields() {
        let config = parse_notification(
            br#"{"topic":"projects/p/topics/topic","id":"9","etag":"x","selfLink":"y","kind":"storage#notification","unknownField":1}"#,
        )
        .unwrap();
        assert_eq!(
            config,
            NotificationConfig {
                topic: "//pubsub.googleapis.com/projects/p/topics/topic"
                    .parse()
                    .unwrap(),
                payload_format: PayloadFormat::JsonApiV1,
                event_types: EventTypes::default(),
                custom_attributes: BTreeMap::new(),
                object_name_prefix: String::new(),
            }
        );
        let config = parse_notification(
            br#"{"topic":"//pubsub.googleapis.com/projects/p/topics/topic","payload_format":"NONE","event_types":["OBJECT_DELETE","OBJECT_FINALIZE"],"custom_attributes":{"a":"1"},"object_name_prefix":"logs/","id":null}"#,
        )
        .unwrap();
        assert_eq!(config.payload_format, PayloadFormat::None);
        assert_eq!(
            config.event_types.iter().collect::<Vec<_>>(),
            [notification::Event::Delete, notification::Event::Finalize]
        );
        assert_eq!(config.custom_attributes["a"], "1");
        assert_eq!(config.object_name_prefix, "logs/");
    }

    #[test]
    fn rejects_malformed_and_unsupported_values() {
        for (body, reason) in [
            (&b"not json"[..], "invalid"),
            (b"[]", "invalid"),
            (b"{}", "required"),
            (br#"{"topic":null}"#, "required"),
            (br#"{"topic":1}"#, "invalid"),
            (br#"{"topic":"topic"}"#, "invalid"),
            (
                br#"{"topic":"projects/p/topics/topic","kind":"storage#bucket"}"#,
                "invalid",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","payload_format":"XML"}"#,
                // Cloud Storage reports an unknown payload format as a missing one.
                "required",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","event_types":"OBJECT_DELETE"}"#,
                "invalid",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","event_types":[1]}"#,
                "invalid",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","event_types":["OBJECT_CHANGE"]}"#,
                "invalid",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","custom_attributes":[]}"#,
                "invalid",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","custom_attributes":{"a":1}}"#,
                "invalid",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","custom_attributes":{"goog":"x"}}"#,
                "invalid",
            ),
            (
                br#"{"topic":"projects/p/topics/topic","object_name_prefix":5}"#,
                "invalid",
            ),
        ] {
            let error = parse_notification(body).unwrap_err();
            assert_eq!(
                (error.status, error.reason),
                (StatusCode::BAD_REQUEST, reason),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn renders_resources_and_lists() {
        let mut config = parse_notification(br#"{"topic":"projects/p/topics/topic"}"#).unwrap();
        let minimal = NotificationRecord {
            id: 3,
            config: config.clone(),
        };
        assert_eq!(
            notification_resource(&minimal, "my.bucket", "http://127.0.0.1:4443/"),
            json!({
                "kind": "storage#notification",
                "id": "3",
                "selfLink": "http://127.0.0.1:4443/storage/v1/b/my.bucket/notificationConfigs/3",
                "topic": "//pubsub.googleapis.com/projects/p/topics/topic",
                "payload_format": "JSON_API_V1",
                "etag": "3"
            })
        );
        config.event_types = [notification::Event::Archive].into_iter().collect();
        config.custom_attributes.insert("k".into(), "v".into());
        config.object_name_prefix = "p/".into();
        let full = NotificationRecord { id: 4, config };
        let value = notification_resource(&full, "b", "http://h");
        assert_eq!(value["event_types"], json!(["OBJECT_ARCHIVE"]));
        assert_eq!(value["custom_attributes"], json!({"k": "v"}));
        assert_eq!(value["object_name_prefix"], "p/");
        assert_eq!(
            notification_list(&[], "b", "http://h"),
            json!({"kind": "storage#notifications"})
        );
        assert_eq!(
            notification_list(&[minimal, full], "b", "http://h")["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}

#[cfg(test)]
mod topic_check_tests {
    use super::*;
    use crate::notification::TopicCheckError;

    #[test]
    fn every_unconfirmed_lookup_is_a_retryable_503_with_a_fixed_body() {
        for error in [
            TopicCheckError::Unexpected(StatusCode::INTERNAL_SERVER_ERROR),
            TopicCheckError::Unreachable(std::io::Error::other("timed out")),
        ] {
            let mapped = topic_unverifiable(&error);
            assert_eq!(mapped.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(mapped.reason, "backendError");
            assert!(!mapped.message.contains("timed out"));
        }
    }

    #[test]
    fn the_log_cause_keeps_the_source_chain() {
        let error = TopicCheckError::Unreachable(std::io::Error::other("connection refused"));
        assert_eq!(
            error_chain(&error),
            "Pub/Sub emulator topic lookup failed: connection refused"
        );
    }
}
