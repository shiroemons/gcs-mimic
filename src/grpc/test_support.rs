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
        generation: crate::object::Generation::for_test(42),
        metageneration: crate::object::Metageneration::for_test(2),
        content_type: "text/plain".into(),
        content_encoding: Some("gzip".into()),
        content_language: Some("en".into()),
        cache_control: Some("private".into()),
        metadata: std::collections::BTreeMap::from([("key".into(), "value".into())]),
        size: 5,
        md5: [3; 16],
        crc32c: 123,
        time_created: now,
        updated: now,
        temporary_hold: crate::object::Hold::Held,
        custom_time: Some("2026-10-20T01:02:03.123456789Z".into()),
        ..crate::storage::test_fixtures::bare_object_record()
    }
}

pub(super) fn storage_with(state: Arc<State>, max_object_bytes: u64) -> super::v2::Storage {
    super::v2::Storage {
        state,
        max_object_bytes,
        compat_profile: crate::profile::CompatProfile::Native,
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

/// Asserts that `status` carries exactly one `google.rpc.ErrorInfo`, naming `reason` in Cloud
/// Storage's domain.
#[track_caller]
pub(super) fn assert_reason(status: &Status, reason: &str) {
    use super::generated::google::rpc::{ErrorInfo, Status as RpcStatus};
    use prost::Message;
    let details = RpcStatus::decode(status.details()).unwrap();
    let [detail] = details.details.as_slice() else {
        panic!("{reason}: {:?}", details.details)
    };
    assert_eq!(detail.type_url, "type.googleapis.com/google.rpc.ErrorInfo");
    let info = ErrorInfo::decode(detail.value.as_slice()).unwrap();
    assert_eq!(
        (info.reason.as_str(), info.domain.as_str()),
        (reason, "storage.googleapis.com")
    );
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

/// The `x-goog-request-params` value a client sends for a call on `bucket`. The emulator
/// refuses a call without the metadata on every Storage v2 RPC, and Cloud Storage was recorded
/// refusing every RPC asked, including QueryWriteStatus and CancelResumableWrite with a
/// readable upload ID (2026-11-18, cases grpc-probe-routing and
/// grpc-probe-inferred-routing).
pub(super) fn bucket_routing(bucket: &str) -> String {
    format!("bucket=projects%2F_%2Fbuckets%2F{bucket}")
}

/// The `x-goog-request-params` value a client sends for a rewrite from `source` to
/// `destination`: both buckets are compared with the request's.
pub(super) fn rewrite_routing(source: &str, destination: &str) -> String {
    format!(
        "source_bucket=projects%2F_%2Fbuckets%2F{source}&{}",
        bucket_routing(destination)
    )
}

/// A Storage v2 request routed by `routing`, the `x-goog-request-params` value.
pub(super) fn routed_request<T>(routing: &str, message: T) -> Request<T> {
    let mut request = Request::new(message);
    request
        .metadata_mut()
        .insert("x-goog-request-params", routing.parse().unwrap());
    request
}

/// A call that carries no routing metadata, as a Storage v1 client sends it.
pub(super) async fn rpc_status<M: prost::Message, R: prost::Message + Default>(
    path: &str,
    token: Option<&str>,
    messages: &[M],
    send: impl AsyncFnOnce(hyper::Request<tonic::body::Body>) -> hyper::Response<tonic::body::Body>,
) -> (Status, Vec<R>) {
    exchange(path, token, None, messages, send).await
}

/// A Storage v2 call routed by `routing`, the `x-goog-request-params` value.
pub(crate) async fn routed_rpc_status<M: prost::Message, R: prost::Message + Default>(
    path: &str,
    token: Option<&str>,
    routing: &str,
    messages: &[M],
    send: impl AsyncFnOnce(hyper::Request<tonic::body::Body>) -> hyper::Response<tonic::body::Body>,
) -> (Status, Vec<R>) {
    exchange(path, token, Some(routing), messages, send).await
}

async fn exchange<M: prost::Message, R: prost::Message + Default>(
    path: &str,
    token: Option<&str>,
    routing: Option<&str>,
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
    if let Some(routing) = routing {
        request = request.header("x-goog-request-params", routing);
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
