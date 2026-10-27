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
