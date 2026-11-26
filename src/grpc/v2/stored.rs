//! Storage v2 object resources kept in catalog JSON columns, so a retried call answers with
//! the resource its first call published.
//!
//! The bytes are the protobuf encoding of `pb::Object`, written as a JSON array of integers.
//! Rows already on disk fix both column forms:
//! - `resumable_sessions.completion` holds the bare array;
//! - `rewrite_sessions.response` keeps it under `resource` as `{"proto": <array>}`, because
//!   the rewrite catalog tells a finished response by its `resource` being a JSON object.

use super::{convert, pb};
use crate::csek::StoredHashes;
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

/// Both stored forms answer the caller that created the object, with the key it supplied (or
/// none), so they show the hashes, as Cloud Storage's completed upload status does even
/// without a key (recorded 2026-11-15).
fn encode(record: &ObjectRecord) -> Result<Value, ObjectError> {
    let bytes = convert::object(record, StoredHashes::Revealed)?.encode_to_vec();
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
        let proto = convert::object(&record, StoredHashes::Revealed).unwrap();
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

    /// The column holds a bare JSON array of the protobuf bytes; these texts are what earlier
    /// releases wrote, so decoding them must keep returning the same object.
    #[test]
    fn completion_text_is_a_bare_array_of_protobuf_bytes() {
        use crate::storage::test_fixtures::populated_object_record;
        // Protobuf map fields come out of `HashMap` iteration, so the bytes of two or more
        // metadata entries differ from run to run; one entry keeps the text deterministic.
        let mut populated = populated_object_record();
        populated.metadata.remove("beta");
        for (record, expected) in [
            (record(), PLAIN_COMPLETION),
            (populated, POPULATED_COMPLETION),
        ] {
            let proto = convert::object(&record, StoredHashes::Revealed).unwrap();
            assert_eq!(completion(&record).unwrap().to_string(), expected);
            let stored: Value = serde_json::from_str(expected).unwrap();
            assert_eq!(from_completion(Some(&stored)).unwrap(), proto);
        }
    }

    const PLAIN_COMPLETION: &str = "[10,14,102,111,108,100,101,114,47,37,50,70,46,116,120,116,18,25,112,114,111,106,101,99,116,115,47,95,47,98,117,99,107,101,116,115,47,98,117,99,107,101,116,24,42,32,2,42,8,83,84,65,78,68,65,82,68,48,5,58,4,103,122,105,112,74,7,112,114,105,118,97,116,101,90,2,101,110,106,10,116,101,120,116,47,112,108,97,105,110,114,0,130,1,23,13,123,0,0,0,18,16,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,3,138,1,0,154,1,0,160,1,1,178,1,12,10,3,107,101,121,18,5,118,97,108,117,101,184,1,0,210,1,11,8,139,251,218,214,6,16,149,154,239,58,218,1,8,67,67,111,81,65,103,61,61,162,2,0]";
    const POPULATED_COMPLETION: &str = "[10,22,102,111,108,100,101,114,47,102,117,108,108,32,111,98,106,101,99,116,46,116,120,116,18,25,112,114,111,106,101,99,116,115,47,95,47,98,117,99,107,101,116,115,47,98,117,99,107,101,116,24,192,196,128,193,193,196,130,3,32,3,42,8,78,69,65,82,76,73,78,69,48,11,58,4,103,122,105,112,66,6,105,110,108,105,110,101,74,7,112,114,105,118,97,116,101,90,2,101,110,98,9,8,172,228,207,170,6,16,184,23,106,10,116,101,120,116,47,112,108,97,105,110,114,11,8,128,226,207,170,6,16,192,169,211,58,120,2,130,1,5,13,123,0,0,0,138,1,6,8,200,227,207,170,6,154,1,6,8,228,226,207,170,6,170,1,11,8,128,230,254,137,7,16,192,169,211,58,178,1,12,10,5,97,108,112,104,97,18,3,111,110,101,184,1,1,202,1,42,10,6,65,69,83,50,53,54,26,32,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,210,1,11,8,165,235,220,202,6,16,149,154,239,58,218,1,16,67,77,68,69,103,77,72,66,120,73,73,68,69,65,77,61,226,1,9,8,172,228,207,170,6,16,184,23,234,1,9,8,204,219,134,171,6,16,184,23,242,1,15,8,2,18,11,8,128,230,254,137,7,16,149,154,239,58,162,2,11,8,128,226,207,170,6,16,128,148,239,58]";

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
