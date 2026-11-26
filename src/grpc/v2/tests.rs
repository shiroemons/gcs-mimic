use super::*;
use crate::grpc::test_support::{self as fixtures, assert_code, registry};
use crate::{
    auth::{Auth, Registry},
    storage::Catalog,
    test_support::TestDir,
};

#[test]
fn authentication_worker_failures_keep_their_source_and_unavailable_status() {
    use std::error::Error as _;
    let failure = || {
        crate::auth::AuthError::Unavailable(crate::storage::failure::BackendCause::from_error(
            std::io::Error::other("verifier failure"),
        ))
    };
    let status = auth_error(failure());
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.source().unwrap().to_string(), "verifier failure");
    assert_eq!(crate::ApiError::from(failure()).status().as_u16(), 503);
}

#[tokio::test]
async fn grpc_auth_metadata_handoff() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut state = State::for_test(catalog, Auth::permissive());
    let mut request = Request::new(pb::GetBucketRequest::default());
    request
        .metadata_mut()
        .insert("authorization", "Basic garbage".parse().unwrap());
    let storage = Storage {
        compat_profile: crate::config::CompatProfile::Native,
        state: Arc::new(state),
        max_object_bytes: 1024 * 1024 * 1024,
    };
    assert!(storage.authenticate(&request).await.unwrap().is_none());
    state = Arc::try_unwrap(storage.state).ok().unwrap();
    state.auth = Auth::strict(Registry::parse(br#"{"identities":[{"id":"a@example.invalid","tokens":["tok-a"],"project_roles":{"p":["roles/storage.admin"]}}]}"#).unwrap());
    let storage = Storage {
        compat_profile: crate::config::CompatProfile::Native,
        state: Arc::new(state),
        max_object_bytes: 1024 * 1024 * 1024,
    };
    assert_code(
        storage.authenticate(&request).await,
        tonic::Code::Unauthenticated,
    );
    request.metadata_mut().clear();
    assert!(matches!(
        storage.authenticate(&request).await.unwrap(),
        Some(Caller::Anonymous)
    ));
    assert_code(
        storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    name: "projects/_/buckets/example-bucket".into(),
                    ..Default::default()
                }),
                Some(Caller::Anonymous),
            )
            .await,
        tonic::Code::Unauthenticated,
    );
    request
        .metadata_mut()
        .insert("authorization", "Bearer tok-a".parse().unwrap());
    let caller = storage.authenticate(&request).await.unwrap().unwrap();
    assert_eq!(caller.principal().unwrap().id(), "a@example.invalid");
    let error = storage.get_bucket(request, Some(caller)).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    storage.state.catalog.close().await.unwrap();
}

/// Sends `message` through the service dispatch, routed by `routing` (the
/// `x-goog-request-params` value) or carrying no routing metadata when `None`, with the `extra`
/// metadata beside it.
async fn routed_call<M: prost::Message>(
    state: &Arc<State>,
    path: &str,
    message: &M,
    routing: Option<&str>,
    extra: &[(&'static str, &str)],
) -> Status {
    let send = async |mut request: hyper::Request<tonic::body::Body>| {
        for (name, value) in extra {
            request.headers_mut().insert(*name, value.parse().unwrap());
        }
        dispatch(
            service(
                state.clone(),
                1024 * 1024,
                crate::config::CompatProfile::Native,
            ),
            request,
        )
        .await
        .unwrap()
    };
    let messages = std::slice::from_ref(message);
    let (status, _): (_, Vec<pb::Object>) = match routing {
        Some(routing) => fixtures::routed_rpc_status(path, None, routing, messages, send).await,
        None => fixtures::rpc_status(path, None, messages, send).await,
    };
    status
}

const GET_OBJECT: &str = "/google.storage.v2.Storage/GetObject";
const LIST_OBJECTS: &str = "/google.storage.v2.Storage/ListObjects";
const WRITE_OBJECT: &str = "/google.storage.v2.Storage/WriteObject";

#[tokio::test]
async fn grpc_object_routing_ignores_billing_and_rejects_conflicting_buckets() {
    let dir = TestDir::new();
    let state = Arc::new(State::for_test(
        Catalog::open(&dir.0).await.unwrap(),
        Auth::permissive(),
    ));
    let get = pb::GetObjectRequest {
        bucket: "projects/_/buckets/example-bucket".into(),
        object: "a".into(),
        ..Default::default()
    };
    let list = pb::ListObjectsRequest {
        parent: get.bucket.clone(),
        ..Default::default()
    };
    let write = pb::WriteObjectRequest {
        first_message: Some(pb::write_object_request::FirstMessage::WriteObjectSpec(
            pb::WriteObjectSpec {
                resource: Some(pb::Object {
                    bucket: get.bucket.clone(),
                    name: "a".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        finish_write: true,
        ..Default::default()
    };
    // The Requester Pays billing project is ignored, so each call reaches the missing bucket.
    let routing = fixtures::bucket_routing("example-bucket");
    let routing = Some(routing.as_str());
    for value in ["", "billing-project"] {
        let billing = [("x-goog-user-project", value)];
        for (method, status) in [
            (
                "GetObject",
                routed_call(&state, GET_OBJECT, &get, routing, &billing).await,
            ),
            (
                "ListObjects",
                routed_call(&state, LIST_OBJECTS, &list, routing, &billing).await,
            ),
            (
                "WriteObject",
                routed_call(&state, WRITE_OBJECT, &write, routing, &billing).await,
            ),
        ] {
            assert_eq!(status.code(), tonic::Code::NotFound, "{method}: {status}");
        }
    }
    // Recorded 2026-11-18 on GetObject (case grpc-probe-routing): the wording of a routed bucket
    // other than the request's, of metadata that routes no bucket, and of a bare bucket ID.
    // WriteObject was not recorded with them; it answers the same by inference.
    for (value, message) in [
        (
            "bucket=projects%2F_%2Fbuckets%2Fother-bucket",
            "The x-goog-request-params must specify the same bucket as the resource in the request. Request bucket was 'example-bucket' but x-goog-request-params specified 'other-bucket'",
        ),
        (
            "project=projects%2Flocal-project",
            "The x-goog-request-params must specify the same bucket as the resource in the request. Request bucket was 'example-bucket' but x-goog-request-params specified ''",
        ),
        (
            "bucket=bad",
            "Invalid bucket name found in x-goog-request-params metadata (bucket=bad). Must be of the form `projects/_/buckets/{bucket_id}`",
        ),
    ] {
        for status in [
            routed_call(&state, GET_OBJECT, &get, Some(value), &[]).await,
            routed_call(&state, WRITE_OBJECT, &write, Some(value), &[]).await,
        ] {
            assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status}");
            assert_eq!(status.message(), message);
            assert!(!status.details().is_empty(), "{value}");
        }
    }
    state.catalog.close().await.unwrap();
}

/// Recorded 2026-11-18 (case grpc-probe-routing): GetObject, GetBucket, ListBuckets and
/// WriteObject without `x-goog-request-params` are refused, while an unknown key beside the
/// bucket (GetObject, GetBucket) or a `routing_token` (GetObject; GetBucket by inference)
/// reaches the handler.
#[tokio::test]
async fn grpc_calls_need_routing_metadata_and_ignore_unknown_keys() {
    let dir = TestDir::new();
    let state = Arc::new(State::for_test(
        Catalog::open(&dir.0).await.unwrap(),
        Auth::permissive(),
    ));
    let bucket = "projects/_/buckets/example-bucket";
    let get_object = pb::GetObjectRequest {
        bucket: bucket.into(),
        object: "a".into(),
        ..Default::default()
    };
    let get_bucket = pb::GetBucketRequest {
        name: bucket.into(),
        ..Default::default()
    };
    let list_buckets = pb::ListBucketsRequest {
        parent: "projects/local-project".into(),
        ..Default::default()
    };
    let write = pb::WriteObjectRequest {
        first_message: Some(pb::write_object_request::FirstMessage::WriteObjectSpec(
            pb::WriteObjectSpec {
                resource: Some(pb::Object {
                    bucket: bucket.into(),
                    name: "a".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        finish_write: true,
        ..Default::default()
    };
    const GET_BUCKET: &str = "/google.storage.v2.Storage/GetBucket";
    for (method, status) in [
        (
            "GetObject",
            routed_call(&state, GET_OBJECT, &get_object, None, &[]).await,
        ),
        (
            "GetBucket",
            routed_call(&state, GET_BUCKET, &get_bucket, None, &[]).await,
        ),
        (
            "ListBuckets",
            routed_call(
                &state,
                "/google.storage.v2.Storage/ListBuckets",
                &list_buckets,
                None,
                &[],
            )
            .await,
        ),
        (
            "WriteObject",
            routed_call(&state, WRITE_OBJECT, &write, None, &[]).await,
        ),
    ] {
        assert_eq!(status.code(), tonic::Code::InvalidArgument, "{method}");
        assert_eq!(
            status.message(),
            "An x-goog-request-params request metadata property must be provided for this request.",
            "{method}"
        );
        assert!(!status.details().is_empty(), "{method}");
    }
    // Each call reaches its handler, which answers the missing bucket.
    for value in [
        "bucket=projects%2F_%2Fbuckets%2Fexample-bucket&gcsmimic_unknown=1",
        "bucket=projects%2F_%2Fbuckets%2Fexample-bucket&routing_token=gcsmimic-probe",
    ] {
        for status in [
            routed_call(&state, GET_OBJECT, &get_object, Some(value), &[]).await,
            routed_call(&state, GET_BUCKET, &get_bucket, Some(value), &[]).await,
        ] {
            assert_eq!(status.code(), tonic::Code::NotFound, "{value}: {status}");
        }
    }
    // Recorded 2026-11-18 (case grpc-probe-resumable-sessions, steps status-malformed and
    // cancel-malformed): an upload ID RPC without the metadata is answered for its malformed
    // upload ID.
    let query = pb::QueryWriteStatusRequest {
        upload_id: "not-an-upload-id".into(),
        ..Default::default()
    };
    let cancel = pb::CancelResumableWriteRequest {
        upload_id: "not-an-upload-id".into(),
    };
    for (method, status) in [
        (
            "QueryWriteStatus",
            routed_call(
                &state,
                "/google.storage.v2.Storage/QueryWriteStatus",
                &query,
                None,
                &[],
            )
            .await,
        ),
        (
            "CancelResumableWrite",
            routed_call(
                &state,
                "/google.storage.v2.Storage/CancelResumableWrite",
                &cancel,
                None,
                &[],
            )
            .await,
        ),
    ] {
        assert_eq!(
            (status.code(), status.message()),
            (
                tonic::Code::InvalidArgument,
                "Invalid upload ID 'not-an-upload-id'."
            ),
            "{method}"
        );
    }
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_names_follow_the_configured_bucket_profile() {
    use crate::config::CompatProfile;
    let dir = TestDir::new();
    let state = Arc::new(State::for_test(
        Catalog::open(&dir.0).await.unwrap(),
        Auth::permissive(),
    ));
    let mut storage = fixtures::storage_with(state.clone(), 1024);
    for resource in ["projects/_/buckets/A_B", "projects/_/buckets/ab"] {
        assert_eq!(
            storage.bucket_id(resource).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            storage.object_name(resource, "a").unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
    }
    storage.compat_profile = CompatProfile::FakeGcsServer;
    let created = pb::storage_server::Storage::create_bucket(
        &storage,
        fixtures::routed_request(
            "project=projects%2Flocal-project",
            pb::CreateBucketRequest {
                parent: "projects/_".into(),
                bucket_id: "ab".into(),
                bucket: Some(pb::Bucket {
                    project: "projects/local-project".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap()
    .into_inner();
    assert_eq!(created.bucket_id, "ab");
    let message = pb::WriteObjectRequest {
        first_message: Some(pb::write_object_request::FirstMessage::WriteObjectSpec(
            pb::WriteObjectSpec {
                resource: Some(pb::Object {
                    bucket: created.name.clone(),
                    name: "a".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
        finish_write: true,
        ..Default::default()
    };
    let routing = fixtures::bucket_routing("ab");
    let (status, _): (_, Vec<pb::WriteObjectResponse>) = fixtures::routed_rpc_status(
        "/google.storage.v2.Storage/WriteObject",
        None,
        &routing,
        &[message],
        async |request| {
            dispatch(
                service(state.clone(), 1024, CompatProfile::FakeGcsServer),
                request,
            )
            .await
            .unwrap()
        },
    )
    .await;
    assert_eq!(status.code(), tonic::Code::Ok, "{status}");
    let object = pb::storage_server::Storage::get_object(
        &storage,
        fixtures::routed_request(
            &routing,
            pb::GetObjectRequest {
                bucket: created.name,
                object: "a".into(),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap()
    .into_inner();
    assert_eq!(object.name, "a");
    assert_eq!(object.size, 0);
    state.catalog.close().await.unwrap();
}

/// Sends one unary call through the service dispatch, as a connection would.
async fn call<M: prost::Message, R: prost::Message + Default>(
    state: &Arc<State>,
    path: &str,
    message: &M,
    token: Option<&str>,
) -> Result<R, tonic::Code> {
    let (status, mut responses) = fixtures::routed_rpc_status(
        path,
        token,
        &fixtures::bucket_routing("iam-bucket"),
        std::slice::from_ref(message),
        async |request| {
            dispatch(
                service(
                    state.clone(),
                    1024 * 1024,
                    crate::config::CompatProfile::Native,
                ),
                request,
            )
            .await
            .unwrap()
        },
    )
    .await;
    if status.code() != tonic::Code::Ok {
        return Err(status.code());
    }
    assert_eq!(responses.len(), 1);
    Ok(responses.pop().unwrap())
}

#[tokio::test]
async fn grpc_storage_control_bucket_iam() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let state = Arc::new(State::for_test(
        catalog,
        Auth::strict(registry(&[
            ("admin", "p", "roles/storage.admin"),
            ("reader", "", ""),
        ])),
    ));
    state
        .catalog
        .insert_bucket(
            crate::json::parse_insert(br#"{"name":"iam-bucket"}"#, "p", state.clock.now()).unwrap(),
        )
        .await
        .unwrap();
    const CONTROL: &str = "/google.storage.control.v2.StorageControl/";
    let resource = "projects/_/buckets/iam-bucket".to_owned();
    let get = iam::GetIamPolicyRequest {
        resource: resource.clone(),
        options: Some(iam::GetPolicyOptions {
            requested_policy_version: 3,
        }),
    };
    let path = format!("{CONTROL}GetIamPolicy");
    let policy: iam::Policy = call(&state, &path, &get, Some("admin")).await.unwrap();
    assert!(!policy.etag.is_empty());
    for (token, code) in [
        (None, tonic::Code::Unauthenticated),
        (Some("reader"), tonic::Code::PermissionDenied),
    ] {
        assert_eq!(
            call::<_, iam::Policy>(&state, &path, &get, token)
                .await
                .unwrap_err(),
            code
        );
    }
    let mut updated = policy.clone();
    updated.bindings.push(iam::Binding {
        role: "roles/storage.objectViewer".into(),
        members: vec!["user:someone@example.invalid".into()],
        condition: None,
    });
    let set = iam::SetIamPolicyRequest {
        resource: resource.clone(),
        policy: Some(updated),
        update_mask: None,
    };
    let path = format!("{CONTROL}SetIamPolicy");
    assert_eq!(
        call::<_, iam::Policy>(&state, &path, &set, Some("reader"))
            .await
            .unwrap_err(),
        tonic::Code::PermissionDenied
    );
    let written: iam::Policy = call(&state, &path, &set, Some("admin")).await.unwrap();
    assert_ne!(written.etag, policy.etag);
    // The request still carries the etag that the write replaced.
    let mut stale = set.clone();
    stale.policy.as_mut().unwrap().bindings.pop();
    assert_eq!(
        call::<_, iam::Policy>(&state, &path, &stale, Some("admin"))
            .await
            .unwrap_err(),
        tonic::Code::FailedPrecondition
    );
    // An etag that is not the eight bytes every issued etag has cannot match the policy either.
    for etag in [vec![1, 2, 3], vec![0; 9]] {
        let mut malformed = set.clone();
        malformed.policy.as_mut().unwrap().etag = etag;
        assert_eq!(
            call::<_, iam::Policy>(&state, &path, &malformed, Some("admin"))
                .await
                .unwrap_err(),
            tonic::Code::FailedPrecondition
        );
    }
    let current: iam::Policy = call(
        &state,
        &format!("{CONTROL}GetIamPolicy"),
        &get,
        Some("admin"),
    )
    .await
    .unwrap();
    assert_eq!(current, written);
    assert!(
        current
            .bindings
            .iter()
            .any(|binding| binding.role == "roles/storage.objectViewer"
                && binding.members == ["user:someone@example.invalid"])
    );
    let test = iam::TestIamPermissionsRequest {
        resource: resource.clone(),
        permissions: vec!["storage.buckets.get".into(), "storage.objects.list".into()],
    };
    let path = format!("{CONTROL}TestIamPermissions");
    let granted: iam::TestIamPermissionsResponse =
        call(&state, &path, &test, Some("admin")).await.unwrap();
    assert_eq!(granted.permissions, test.permissions);
    let granted: iam::TestIamPermissionsResponse =
        call(&state, &path, &test, Some("reader")).await.unwrap();
    assert!(granted.permissions.is_empty());
    for resource in [
        "projects/_/buckets/iam-bucket/managedFolders/folder",
        "projects/_/buckets/iam-bucket/objects/object",
    ] {
        let test = iam::TestIamPermissionsRequest {
            resource: resource.into(),
            permissions: vec!["storage.objects.get".into()],
        };
        assert_eq!(
            call::<_, iam::TestIamPermissionsResponse>(&state, &path, &test, Some("admin"))
                .await
                .unwrap_err(),
            tonic::Code::Unimplemented
        );
    }
    assert_eq!(
        call::<_, iam::Policy>(&state, &format!("{CONTROL}GetFolder"), &get, Some("admin"))
            .await
            .unwrap_err(),
        tonic::Code::Unimplemented
    );
    state.catalog.close().await.unwrap();
}
