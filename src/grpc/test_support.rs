//! Fixtures shared by gRPC adapters and authorization tests.

use crate::{
    auth::{Caller, Registry},
    server::State,
};
use std::sync::Arc;
use tonic::{Code, Request, Status};

pub(super) fn object_record(
    bucket: &str,
    now: std::time::SystemTime,
) -> crate::object::ObjectRecord {
    crate::object::ObjectRecord {
        bucket: bucket.into(),
        name: "folder/%2F.txt".into(),
        generation: 42,
        metageneration: 2,
        owner: None,
        content_type: "text/plain".into(),
        content_encoding: Some("gzip".into()),
        content_disposition: None,
        content_language: Some("en".into()),
        cache_control: Some("private".into()),
        metadata: std::collections::BTreeMap::from([("key".into(), "value".into())]),
        size: 5,
        md5: [3; 16],
        md5_availability: Default::default(),
        crc32c: 123,
        component_count: None,
        storage_class: crate::bucket::StorageClass::Standard,
        time_created: now,
        time_storage_class_updated: None,
        updated: now,
        time_deleted: None,
        soft_delete_time: None,
        hard_delete_time: None,
        event_based_hold: crate::object::Hold::Unset,
        temporary_hold: crate::object::Hold::Held,
        event_based_hold_release_time: None,
        custom_time: Some("2026-10-20T01:02:03.123456789Z".into()),
        retention_expiration_time: None,
        retention: None,
        customer_key_sha256: None,
    }
}

pub(super) fn storage_with(state: Arc<State>, max_object_bytes: u64) -> super::v2::Storage {
    super::v2::Storage {
        state,
        max_object_bytes,
        compat_profile: crate::config::CompatProfile::Native,
    }
}

pub(super) fn registry(entries: &[(&str, &str, &str)]) -> Registry {
    let identities: Vec<_> = entries
        .iter()
        .map(|(token, project, role)| {
            let mut identity =
                serde_json::json!({"id": format!("{token}@example.invalid"), "tokens": [token]});
            if !role.is_empty() {
                identity["project_roles"] = serde_json::json!({(*project): [role]});
            }
            identity
        })
        .collect();
    Registry::parse(&serde_json::to_vec(&serde_json::json!({"identities": identities})).unwrap())
        .unwrap()
}

pub(super) async fn caller<'a>(storage: &'a super::v2::Storage, token: &str) -> Option<Caller<'a>> {
    let mut request = Request::new(());
    request
        .metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());
    storage.authenticate(&request).await.unwrap()
}

#[track_caller]
pub(super) fn assert_code<T>(result: Result<T, Status>, code: Code) -> Status {
    match result {
        Err(status) => {
            assert_eq!(status.code(), code);
            status
        }
        Ok(_) => panic!("expected gRPC status {code:?}, got success"),
    }
}

/// An unsupported option fails fast with ErrorInfo naming it, so an SDK neither
/// retries nor mistakes the rejection for a missing resource.
#[track_caller]
pub(super) fn assert_unsupported<T>(result: Result<T, Status>, option: &str) {
    let status = assert_code(result, Code::Unimplemented);
    assert!(status.message().contains(option), "{option}: {status}");
    assert!(!status.details().is_empty(), "{option}");
}

/// A refused request answers INVALID_ARGUMENT with one `BadRequest` violation that repeats
/// `message`, as Cloud Storage does.
#[track_caller]
pub(super) fn assert_bad_request<T>(result: Result<T, Status>, message: &str) {
    use super::generated::google::rpc::{BadRequest, Status as RpcStatus};
    use prost::Message;
    let status = assert_code(result, Code::InvalidArgument);
    assert_eq!(status.message(), message);
    let details = RpcStatus::decode(status.details()).unwrap();
    let violations = BadRequest::decode(details.details[0].value.as_slice()).unwrap();
    assert_eq!(violations.field_violations[0].description, message);
}

/// Storage v2 request parameters carrying `key` as raw bytes, as the SDKs send it.
pub(super) fn key_params(
    key: &[u8; 32],
) -> Option<crate::grpc::generated::google::storage::v2::CommonObjectRequestParams> {
    Some(
        crate::grpc::generated::google::storage::v2::CommonObjectRequestParams {
            encryption_algorithm: crate::csek::ALGORITHM.into(),
            encryption_key_bytes: key.to_vec(),
            encryption_key_sha256_bytes: crate::csek::key_sha256(key).to_vec(),
        },
    )
}

/// Stores `data` at bucket/name under `key` (or unencrypted), as an upload through any API
/// would, so a test can address an encrypted object over gRPC.
pub(super) async fn put_keyed(
    catalog: &crate::storage::Catalog,
    bucket: &str,
    name: &str,
    data: &[u8],
    key: Option<&[u8; 32]>,
) -> crate::object::ObjectRecord {
    let mut upload = crate::storage::test_fixtures::permissive_upload(bucket, name, u64::MAX);
    upload.metadata.customer_key_sha256 = key.map(crate::csek::key_sha256);
    catalog
        .put_object(
            upload,
            crate::storage::test_fixtures::body(vec![bytes::Bytes::copy_from_slice(data)]),
        )
        .await
        .unwrap()
}

/// A customer-supplied key failure answered with the recorded gRPC status and wording.
#[track_caller]
pub(super) fn assert_customer_encryption<T>(
    result: Result<T, Status>,
    error: crate::csek::CsekError,
) {
    let status = assert_code(result, Code::InvalidArgument);
    let expected = super::v2::status::customer_encryption(error);
    assert_eq!(status.message(), expected.message(), "{error:?}");
    assert_eq!(status.details(), expected.details(), "{error:?}");
}

pub(super) fn mask(paths: &[&str]) -> Option<prost_types::FieldMask> {
    Some(prost_types::FieldMask {
        paths: paths.iter().map(|path| (*path).into()).collect(),
    })
}

pub(super) async fn rpc<M: prost::Message, R: prost::Message + Default>(
    path: &str,
    token: Option<&str>,
    messages: &[M],
    send: impl AsyncFnOnce(hyper::Request<tonic::body::Body>) -> hyper::Response<tonic::body::Body>,
) -> (Code, Vec<R>) {
    let (status, messages) = rpc_status(path, token, messages, send).await;
    (status.code(), messages)
}

pub(super) async fn rpc_status<M: prost::Message, R: prost::Message + Default>(
    path: &str,
    token: Option<&str>,
    messages: &[M],
    send: impl AsyncFnOnce(hyper::Request<tonic::body::Body>) -> hyper::Response<tonic::body::Body>,
) -> (Status, Vec<R>) {
    use http_body_util::BodyExt;
    let mut body = Vec::new();
    for message in messages {
        let encoded = message.encode_to_vec();
        body.push(0);
        body.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&encoded);
    }
    let mut request = hyper::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .body(tonic::body::Body::new(http_body_util::Full::new(
            bytes::Bytes::from(body),
        )))
        .unwrap();
    let response = send(request).await;
    let mut status = Status::from_header_map(response.headers());
    let mut body = response.into_body();
    let mut encoded = Vec::new();
    while let Some(frame) = body.frame().await {
        match frame.unwrap().into_data() {
            Ok(data) => {
                assert!(encoded.len() + data.len() <= 4 * 1024 * 1024);
                encoded.extend_from_slice(&data);
            }
            Err(frame) => {
                if let Ok(trailers) = frame.into_trailers() {
                    status = Status::from_header_map(&trailers);
                }
            }
        }
    }
    let mut messages = Vec::new();
    let mut remaining = encoded.as_slice();
    while !remaining.is_empty() {
        assert!(remaining.len() >= 5);
        assert_eq!(remaining[0], 0);
        let size =
            usize::try_from(u32::from_be_bytes(remaining[1..5].try_into().unwrap())).unwrap();
        assert!(remaining.len() >= 5 + size);
        messages.push(R::decode(&remaining[5..5 + size]).unwrap());
        remaining = &remaining[5 + size..];
    }
    (status.expect("terminal grpc-status"), messages)
}
