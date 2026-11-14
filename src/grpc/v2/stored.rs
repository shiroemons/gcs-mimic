//! Storage v2 object resources kept in catalog JSON columns, so a retried call answers with
//! the resource its first call published.
//!
//! The bytes are the protobuf encoding of `pb::Object`, written as a JSON array of integers.
//! Rows already on disk fix both column forms:
//! - `resumable_sessions.completion` holds the bare array;
//! - `rewrite_sessions.response` keeps it under `resource` as `{"proto": <array>}`, because
//!   the rewrite catalog tells a finished response by its `resource` being a JSON object.

use super::{convert, pb};
use crate::object::{ObjectError, ObjectRecord};
use prost::Message;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::Arc;
use tonic::Status;

const REWRITE_KEY: &str = "proto";

/// Why a stored resource cannot be read back.
#[derive(Debug)]
enum Unreadable {
    NotBytes(serde_json::Error),
    NotObject(prost::DecodeError),
    MissingKey,
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotBytes(_) => "stored resource is not an array of bytes",
            Self::NotObject(_) => "stored bytes are not a Storage v2 object",
            Self::MissingKey => "stored rewrite response has no proto key",
        })
    }
}

impl std::error::Error for Unreadable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotBytes(error) => Some(error),
            Self::NotObject(error) => Some(error),
            Self::MissingKey => None,
        }
    }
}

fn encode(record: &ObjectRecord) -> Result<Value, ObjectError> {
    let bytes = convert::object(record)?.encode_to_vec();
    Ok(Value::Array(bytes.into_iter().map(Value::from).collect()))
}

fn decode(value: &Value) -> Result<pb::Object, Unreadable> {
    let bytes = Vec::<u8>::deserialize(value).map_err(Unreadable::NotBytes)?;
    pb::Object::decode(bytes.as_slice()).map_err(Unreadable::NotObject)
}

/// An internal error that keeps its cause for logs; the client sees only `message`.
fn internal(message: &'static str, cause: Unreadable) -> Status {
    let mut status = Status::internal(message);
    status.set_source(Arc::new(cause));
    status
}

/// The `render` callback of `Catalog::write_resumable_session`.
pub(super) fn completion(record: &ObjectRecord) -> Result<Value, ObjectError> {
    encode(record)
}

/// The `render` callback of `Catalog::continue_rewrite`.
pub(super) fn rewrite_resource(record: &ObjectRecord) -> Result<Value, ObjectError> {
    Ok(Value::Object(Map::from_iter([(
        REWRITE_KEY.to_owned(),
        encode(record)?,
    )])))
}

pub(super) fn from_completion(value: Option<&Value>) -> Result<pb::Object, Status> {
    let value = value.ok_or_else(|| Status::internal("Missing upload completion"))?;
    decode(value).map_err(|cause| internal("Invalid upload completion", cause))
}

pub(super) fn from_rewrite_resource(value: &Value) -> Result<pb::Object, Status> {
    let bytes = value
        .get(REWRITE_KEY)
        .ok_or_else(|| internal("Invalid stored rewrite response", Unreadable::MissingKey))?;
    decode(bytes).map_err(|cause| internal("Invalid stored rewrite response", cause))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record() -> ObjectRecord {
        crate::grpc::test_support::object_record("bucket", std::time::UNIX_EPOCH)
    }

    #[test]
    fn stored_resources_keep_the_bytes_earlier_releases_wrote() {
        let record = record();
        let proto = convert::object(&record).unwrap();
        // The encoders these columns were first written with.
        let old_completion = serde_json::to_value(proto.encode_to_vec())
            .unwrap()
            .to_string();
        let old_rewrite = json!({"proto": proto.encode_to_vec()}).to_string();

        assert_eq!(completion(&record).unwrap().to_string(), old_completion);
        let rewrite = rewrite_resource(&record).unwrap();
        assert!(rewrite.is_object());
        assert_eq!(rewrite.to_string(), old_rewrite);

        let stored: Value = serde_json::from_str(&old_completion).unwrap();
        assert_eq!(from_completion(Some(&stored)).unwrap(), proto);
        let stored: Value = serde_json::from_str(&old_rewrite).unwrap();
        assert_eq!(from_rewrite_resource(&stored).unwrap(), proto);
    }

    fn assert_internal(result: Result<pb::Object, Status>, message: &str) -> Status {
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::Internal);
        assert_eq!(status.message(), message);
        status
    }

    #[test]
    fn unreadable_resources_are_internal_errors_with_their_cause() {
        assert!(
            std::error::Error::source(&assert_internal(
                from_completion(None),
                "Missing upload completion"
            ))
            .is_none()
        );
        for value in [
            json!({"proto": [1]}),
            json!("bytes"),
            json!([256]),
            json!([-1]),
            json!([255]),
        ] {
            let status =
                assert_internal(from_completion(Some(&value)), "Invalid upload completion");
            assert!(std::error::Error::source(&status).is_some(), "{value}");
        }
        for value in [
            json!({}),
            json!([1]),
            json!({"proto": "bytes"}),
            json!({"proto": [256]}),
            json!({"proto": [255]}),
        ] {
            let status = assert_internal(
                from_rewrite_resource(&value),
                "Invalid stored rewrite response",
            );
            assert!(std::error::Error::source(&status).is_some(), "{value}");
        }
    }
}
