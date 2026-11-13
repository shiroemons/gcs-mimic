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
    assert_eq!(crate::json::JsonError::from(failure()).status.as_u16(), 503);
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

async fn routed_call<M: prost::Message>(
    state: &Arc<State>,
    path: &str,
    message: &M,
    header: (&'static str, &'static str),
) -> Status {
    let (status, _): (_, Vec<pb::Object>) = fixtures::rpc_status(
        path,
        None,
        std::slice::from_ref(message),
        async |mut request| {
            request
                .headers_mut()
                .insert(header.0, header.1.parse().unwrap());
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
    status
}

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
    for header in [
        ("x-goog-user-project", ""),
        ("x-goog-user-project", "billing-project"),
    ] {
        for status in [
            routed_call(&state, "/google.storage.v2.Storage/GetObject", &get, header).await,
            routed_call(
                &state,
                "/google.storage.v2.Storage/ListObjects",
                &list,
                header,
            )
            .await,
            routed_call(
                &state,
                "/google.storage.v2.Storage/WriteObject",
                &write,
                header,
            )
            .await,
        ] {
            assert_eq!(status.code(), tonic::Code::NotFound, "{status}");
        }
    }
    for value in ["bucket=projects%2F_%2Fbuckets%2Fother-bucket", "bucket=bad"] {
        for status in [
            routed_call(
                &state,
                "/google.storage.v2.Storage/GetObject",
                &get,
                ("x-goog-request-params", value),
            )
            .await,
            routed_call(
                &state,
                "/google.storage.v2.Storage/WriteObject",
                &write,
                ("x-goog-request-params", value),
            )
            .await,
        ] {
            assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status}");
        }
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
        Request::new(pb::CreateBucketRequest {
            parent: "projects/_".into(),
            bucket_id: "ab".into(),
            bucket: Some(pb::Bucket {
                project: "projects/local-project".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
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
    let (status, _): (_, Vec<pb::WriteObjectResponse>) = fixtures::rpc_status(
        "/google.storage.v2.Storage/WriteObject",
        None,
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
        Request::new(pb::GetObjectRequest {
            bucket: created.name,
            object: "a".into(),
            ..Default::default()
        }),
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
    let (code, mut responses) = fixtures::rpc(
        path,
        token,
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
    if code != tonic::Code::Ok {
        return Err(code);
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
