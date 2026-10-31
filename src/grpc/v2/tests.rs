use super::*;
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    storage::Catalog,
    test_support::TestDir,
};
use std::sync::atomic::AtomicBool;

#[tokio::test]
async fn grpc_auth_metadata_handoff() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut state = State {
        ready: AtomicBool::new(true),
        clock: Arc::new(SystemClock),
        catalog,
        api_base_url: "http://localhost:4443".into(),
        default_location: "US".into(),
        auth: Auth::permissive(),
    };
    let mut request = Request::new(pb::GetBucketRequest::default());
    request
        .metadata_mut()
        .insert("authorization", "Basic garbage".parse().unwrap());
    let storage = Storage {
        state: Arc::new(state),
        max_object_bytes: 1024 * 1024 * 1024,
    };
    assert!(storage.authenticate(&request).unwrap().is_none());
    state = Arc::try_unwrap(storage.state).ok().unwrap();
    state.auth = Auth::strict(Registry::parse(br#"{"identities":[{"id":"a@example.invalid","tokens":["tok-a"],"project_roles":{"p":["roles/storage.admin"]}}]}"#).unwrap());
    let storage = Storage {
        state: Arc::new(state),
        max_object_bytes: 1024 * 1024 * 1024,
    };
    assert_eq!(
        storage.authenticate(&request).unwrap_err().code(),
        tonic::Code::Unauthenticated
    );
    request.metadata_mut().clear();
    assert!(matches!(
        storage.authenticate(&request).unwrap(),
        Some(Caller::Anonymous)
    ));
    assert_eq!(
        storage
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    name: "projects/_/buckets/example-bucket".into(),
                    ..Default::default()
                }),
                Some(Caller::Anonymous)
            )
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unauthenticated
    );
    request
        .metadata_mut()
        .insert("authorization", "Bearer tok-a".parse().unwrap());
    let caller = storage.authenticate(&request).unwrap().unwrap();
    assert_eq!(caller.principal().unwrap().id(), "a@example.invalid");
    let error = storage.get_bucket(request, Some(caller)).await.unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    storage.state.catalog.close().await.unwrap();
}

/// Sends one unary call through the service dispatch, as a connection would.
async fn call<M: prost::Message, R: prost::Message + Default>(
    state: &Arc<State>,
    path: &str,
    message: &M,
    token: Option<&str>,
) -> Result<R, tonic::Code> {
    use http_body_util::BodyExt;
    let payload = message.encode_to_vec();
    let mut frame = vec![0];
    frame.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(&payload);
    let mut request = hyper::Request::builder()
        .method("POST")
        .uri(format!("http://localhost{path}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = request
        .body(tonic::body::Body::new(http_body_util::Full::new(
            bytes::Bytes::from(frame),
        )))
        .unwrap();
    let response = dispatch(service(state.clone(), 1024 * 1024), request)
        .await
        .unwrap();
    let headers = response.headers().clone();
    let collected = response.into_body().collect().await.unwrap();
    let trailers = collected.trailers().cloned().unwrap_or_default();
    let status = trailers
        .get("grpc-status")
        .or_else(|| headers.get("grpc-status"))
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<i32>()
        .unwrap();
    if status != 0 {
        return Err(tonic::Code::from_i32(status));
    }
    let body = collected.to_bytes();
    Ok(R::decode(&body[5..]).unwrap())
}

#[tokio::test]
async fn grpc_storage_control_bucket_iam() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(SystemClock),
        catalog,
        api_base_url: "http://localhost:4443".into(),
        default_location: "US".into(),
        auth: Auth::strict(Registry::parse(br#"{"identities":[{"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"p":["roles/storage.admin"]}},{"id":"reader@example.invalid","tokens":["reader"]}]}"#).unwrap()),
    });
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
