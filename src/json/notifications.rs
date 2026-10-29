//! JSON wire adapter for `storage.notifications.*` (`b/{bucket}/notificationConfigs`).
//!
//! Routing, request validation and rendering only; persistence and authorization live in the
//! catalog. Configurations are stored and read back, but no Pub/Sub message is ever published.

use super::{Fields, JsonError, buckets::encode_segment, query};
use crate::{
    config::CompatProfile,
    notification::{self, NotificationConfig, NotificationRecord},
};
use hyper::{Method, StatusCode, Uri};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationRequest {
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
    pub fn bucket(&self) -> &str {
        match self {
            Self::Insert { bucket }
            | Self::List { bucket }
            | Self::Get { bucket, .. }
            | Self::Delete { bucket, .. } => bucket,
        }
    }
}

/// Match `b/{bucket}/notificationConfigs[/{notification}]` before any other bucket route.
pub fn route(
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
                let mut error = JsonError::new(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "methodNotAllowed",
                    format!(
                        "HTTP method is not supported for this notification endpoint; use {allowed}"
                    ),
                );
                error.allow = Some(allowed);
                return Err(error);
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

/// Parse a notifications.insert body. Output-only `id`, `etag` and `selfLink` are ignored,
/// following the server-assigned semantics of those Discovery fields; other unknown fields
/// follow the bucket insert ignored-input rule.
pub fn parse_notification(bytes: &[u8]) -> Result<NotificationConfig, JsonError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid JSON notification resource",
        )
    })?;
    let body = value
        .as_object()
        .ok_or_else(|| invalid("Notification resource must be a JSON object"))?;
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
    let topic = string("topic")?.ok_or_else(|| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "required",
            "Missing required topic; set topic to //pubsub.googleapis.com/projects/PROJECT/topics/TOPIC",
        )
    })?;
    let topic = notification::normalize_topic(topic).map_err(|error| invalid(error.to_string()))?;
    let payload_format = string("payload_format")?
        .unwrap_or("JSON_API_V1")
        .to_owned();
    notification::validate_payload_format(&payload_format)
        .map_err(|error| invalid(error.to_string()))?;
    let event_types = match field("event_types") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| invalid("event_types must be an array of strings"))?
            .iter()
            .map(|event| {
                event
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("event_types must be an array of strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    notification::validate_event_types(&event_types).map_err(|error| invalid(error.to_string()))?;
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
    notification::validate_custom_attributes(&custom_attributes)
        .map_err(|error| invalid(error.to_string()))?;
    let object_name_prefix = string("object_name_prefix")?.unwrap_or_default().to_owned();
    notification::validate_object_name_prefix(&object_name_prefix)
        .map_err(|error| invalid(error.to_string()))?;
    Ok(NotificationConfig {
        topic,
        payload_format,
        event_types,
        custom_attributes,
        object_name_prefix,
    })
}

/// Render a stored configuration. Configurations are immutable, so the ID doubles as the etag.
pub fn notification_resource(record: &NotificationRecord, bucket: &str, base_url: &str) -> Value {
    let id = record.id.to_string();
    let config = &record.config;
    let mut value = json!({
        "kind": "storage#notification",
        "id": id,
        "selfLink": format!(
            "{}/storage/v1/b/{}/notificationConfigs/{id}",
            base_url.trim_end_matches('/'),
            encode_segment(bucket)
        ),
        "topic": config.topic,
        "payload_format": config.payload_format,
        "etag": id,
    });
    if !config.event_types.is_empty() {
        value["event_types"] = json!(config.event_types);
    }
    if !config.custom_attributes.is_empty() {
        value["custom_attributes"] = Value::Object(
            config
                .custom_attributes
                .iter()
                .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
                .collect::<Map<_, _>>(),
        );
    }
    if !config.object_name_prefix.is_empty() {
        value["object_name_prefix"] = config.object_name_prefix.as_str().into();
    }
    value
}

pub fn notification_list(records: &[NotificationRecord], bucket: &str, base_url: &str) -> Value {
    let mut value = json!({"kind": "storage#notifications"});
    if !records.is_empty() {
        value["items"] = records
            .iter()
            .map(|record| notification_resource(record, bucket, base_url))
            .collect();
    }
    value
}

pub fn notification_not_found() -> JsonError {
    JsonError::new(
        StatusCode::NOT_FOUND,
        "notFound",
        "Notification configuration not found; list b/BUCKET/notificationConfigs for valid IDs",
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
                "/storage/v1/b/bucket/notificationConfigs?userProject=p",
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
                topic: "//pubsub.googleapis.com/projects/p/topics/topic".into(),
                payload_format: "JSON_API_V1".into(),
                event_types: Vec::new(),
                custom_attributes: BTreeMap::new(),
                object_name_prefix: String::new(),
            }
        );
        let config = parse_notification(
            br#"{"topic":"//pubsub.googleapis.com/projects/p/topics/topic","payload_format":"NONE","event_types":["OBJECT_DELETE","OBJECT_FINALIZE"],"custom_attributes":{"a":"1"},"object_name_prefix":"logs/","id":null}"#,
        )
        .unwrap();
        assert_eq!(config.payload_format, "NONE");
        assert_eq!(config.event_types, ["OBJECT_DELETE", "OBJECT_FINALIZE"]);
        assert_eq!(config.custom_attributes["a"], "1");
        assert_eq!(config.object_name_prefix, "logs/");
    }

    #[test]
    fn rejects_malformed_and_unsupported_values() {
        for (body, reason) in [
            (&b"not json"[..], "parseError"),
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
                "invalid",
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
        config.event_types = vec!["OBJECT_ARCHIVE".into()];
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
