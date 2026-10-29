use super::pb::storage_server::Storage as _;
use super::*;
use crate::{
    auth::{Auth, Registry},
    bucket::BucketRecord,
    clock::SystemClock,
    storage::Catalog,
    test_support::TestDir,
};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, UNIX_EPOCH},
};

fn bucket(name: &str, project: &str) -> BucketRecord {
    BucketRecord {
        name: name.into(),
        project: project.into(),
        location: "US".into(),
        location_type: "multi-region".into(),
        storage_class: "STANDARD".into(),
        autoclass: None,
        labels: Default::default(),
        versioning_enabled: false,
        default_event_based_hold: false,
        public_access_prevention_enforced: false,
        uniform_bucket_level_access_locked_time: None,
        lifecycle: Vec::new(),
        retention_policy: None,
        soft_delete_policy: None,
        object_retention_enabled: false,
        cors: Vec::new(),
        metageneration: 1,
        time_created: UNIX_EPOCH + Duration::from_secs(1),
        updated: UNIX_EPOCH + Duration::from_secs(1),
    }
}

async fn storage(dir: &TestDir, auth: Auth) -> Storage {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for (name, project) in [("list-a", "p2"), ("list-b", "p1"), ("list-c", "p3")] {
        catalog.insert_bucket(bucket(name, project)).await.unwrap();
    }
    Storage {
        state: Arc::new(State {
            ready: AtomicBool::new(true),
            clock: Arc::new(SystemClock),
            catalog,
            api_base_url: "http://localhost:4443".into(),
            default_location: "US".into(),
            auth,
        }),
        max_object_bytes: 1024 * 1024,
        compat_profile: crate::config::CompatProfile::LegacyEmulator,
    }
}

fn list(
    project: &str,
    token: Option<&str>,
    max_results: i32,
    page_token: &str,
) -> Request<pb::ListBucketsRequest> {
    let mut request = Request::new(pb::ListBucketsRequest {
        project: project.into(),
        prefix: "list-".into(),
        max_results,
        page_token: page_token.into(),
        ..Default::default()
    });
    if let Some(token) = token {
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    request
}

fn names(response: &pb::ListBucketsResponse) -> Vec<&str> {
    response
        .items
        .iter()
        .map(|item| item.name.as_str())
        .collect()
}

#[tokio::test]
async fn list_buckets_without_project_spans_projects_with_paging() {
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    let first = storage
        .list_buckets(list("", None, 2, ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&first), ["list-a", "list-b"]);
    assert!(!first.next_page_token.is_empty());
    // An all-projects cursor is not a project cursor, and vice versa.
    assert_eq!(
        storage
            .list_buckets(list("p1", None, 2, &first.next_page_token))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::InvalidArgument
    );
    let rest = storage
        .list_buckets(list("", None, 2, &first.next_page_token))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&rest), ["list-c"]);
    assert!(rest.next_page_token.is_empty());
    let project_token = json::encode_page_token("p1", "list-", "list-a").unwrap();
    assert_eq!(
        storage
            .list_buckets(list("", None, 2, &project_token))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::InvalidArgument
    );
    let scoped = storage
        .list_buckets(list("p1", None, 0, ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&scoped), ["list-b"]);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn strict_list_buckets_without_project_only_sees_listable_projects() {
    let dir = TestDir::new();
    let registry = Registry::parse(
        br#"{"identities":[
            {"id":"two@example.invalid","tokens":["tok-two"],"project_roles":{"p1":["roles/storage.admin"],"p2":["roles/storage.admin"],"p3":["roles/storage.objectViewer"]}},
            {"id":"none@example.invalid","tokens":["tok-none"]}
        ]}"#,
    )
    .unwrap();
    let storage = storage(&dir, Auth::strict(registry)).await;
    let first = storage
        .list_buckets(list("", Some("tok-two"), 1, ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&first), ["list-a"]);
    let rest = storage
        .list_buckets(list("", Some("tok-two"), 1000, &first.next_page_token))
        .await
        .unwrap()
        .into_inner();
    // objectViewer lacks storage.buckets.list, so the p3 bucket stays hidden.
    assert_eq!(names(&rest), ["list-b"]);
    assert!(rest.next_page_token.is_empty());
    assert_eq!(
        storage
            .list_buckets(list("", Some("tok-none"), 0, ""))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::PermissionDenied
    );
    assert_eq!(
        storage
            .list_buckets(list("", None, 0, ""))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::Unauthenticated
    );
    assert_eq!(
        storage
            .list_buckets(list("p3", Some("tok-two"), 0, ""))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::PermissionDenied
    );
    storage.state.catalog.close().await.unwrap();
}
