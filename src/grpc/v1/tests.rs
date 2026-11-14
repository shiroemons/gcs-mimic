use super::pb::storage_server::Storage as _;
use super::*;
use crate::grpc::test_support::{self as fixtures, assert_code};
use crate::{
    auth::{Auth, Registry},
    bucket::BucketRecord,
    storage::{Catalog, test_fixtures::PERMISSIVE},
    test_support::TestDir,
};
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn typed_object_metadata_keeps_replace_resets_and_patch_omissions() {
    for mode in [json::WriteMode::Patch, json::WriteMode::Replace] {
        let update = metadata(pb::Object::default(), mode).unwrap();
        assert_eq!(update.write, mode);
        assert_eq!(
            update.content_type,
            (mode == json::WriteMode::Replace).then_some(None)
        );
        assert_eq!(update.metadata.is_some(), mode == json::WriteMode::Replace);
        assert_eq!(update.event_based_hold, None);
        assert_eq!(update.temporary_hold, None);
    }
    let update = metadata(
        pb::Object {
            event_based_hold: Some(false),
            temporary_hold: true,
            ..Default::default()
        },
        json::WriteMode::Patch,
    )
    .unwrap();
    assert_eq!(update.event_based_hold, Some(false));
    assert_eq!(update.temporary_hold, Some(true));
}

fn bucket(name: &str, project: &str) -> BucketRecord {
    BucketRecord {
        time_created: UNIX_EPOCH + Duration::from_secs(1),
        updated: UNIX_EPOCH + Duration::from_secs(1),
        ..crate::storage::test_fixtures::bucket(name, project)
    }
}

async fn storage(dir: &TestDir, auth: Auth) -> Storage {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for (name, project) in [("list-a", "p2"), ("list-b", "p1"), ("list-c", "p3")] {
        catalog.insert_bucket(bucket(name, project)).await.unwrap();
    }
    Storage {
        state: Arc::new(crate::server::State::for_test(catalog, auth)),
        max_object_bytes: 1024 * 1024,
        compat_profile: crate::config::CompatProfile::FakeGcsServer,
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
async fn nonempty_bucket_deletion_uses_the_shared_grpc_status() {
    use crate::grpc::generated::google::storage::v2::{
        self as v2_pb, storage_server::Storage as V2Storage,
    };
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.send(Ok(crate::object::UploadChunk::End)).await.unwrap();
    drop(tx);
    storage
        .state
        .catalog
        .put_object(
            crate::storage::test_fixtures::permissive_upload("list-a", "object", 1024),
            rx,
        )
        .await
        .unwrap();
    let legacy = assert_code(
        storage
            .delete_bucket(Request::new(pb::DeleteBucketRequest {
                bucket: "list-a".into(),
                ..Default::default()
            }))
            .await,
        tonic::Code::FailedPrecondition,
    );
    assert_eq!(
        legacy.message(),
        "Bucket is not empty; delete its objects and versions before deleting the bucket"
    );
    let modern = fixtures::storage_with(storage.state.clone(), 1024);
    let modern = assert_code(
        V2Storage::delete_bucket(
            &modern,
            Request::new(v2_pb::DeleteBucketRequest {
                name: "projects/_/buckets/list-a".into(),
                ..Default::default()
            }),
        )
        .await,
        tonic::Code::FailedPrecondition,
    );
    assert_eq!(
        modern.message(),
        "Bucket is not empty; delete its objects and versions before deleting the bucket"
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn object_listing_preserves_encoded_query_and_uri_limits() {
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    let fixed_query = "prefix=&delimiter=&pageToken=&maxResults=1000&versions=false&includeTrailingDelimiter=false";
    for (prefix, expected) in [
        ("a".repeat(json::MAX_QUERY_BYTES - fixed_query.len()), None),
        (
            "a".repeat(json::MAX_QUERY_BYTES - fixed_query.len() + 1),
            Some("Invalid object listing or page token; use the token with its original query"),
        ),
        (
            "/".repeat(5500),
            Some("Invalid object listing or page token; use the token with its original query"),
        ),
        ("/".repeat(22000), Some("Invalid list request")),
    ] {
        let uri = format!("/storage/v1/b/list-a/o?prefix={}&delimiter=&pageToken=&maxResults=1000&versions=false&includeTrailingDelimiter=false", crate::auth::signature::encode_query_component(&prefix)).parse::<hyper::Uri>();
        match expected {
            None => assert!(json::parse_query(uri.as_ref().unwrap().query().unwrap()).is_ok()),
            Some("Invalid list request") => assert!(uri.is_err()),
            Some(_) => assert!(json::parse_query(uri.as_ref().unwrap().query().unwrap()).is_err()),
        }
        let actual = storage
            .list_objects(Request::new(pb::ListObjectsRequest {
                bucket: "list-a".into(),
                prefix,
                ..Default::default()
            }))
            .await;
        if let Some(expected) = expected {
            assert_eq!(
                assert_code(actual, tonic::Code::InvalidArgument).message(),
                expected
            );
        } else {
            assert!(actual.unwrap().into_inner().items.is_empty());
        }
    }
    let fixed_uri_len = "/storage/v1/b/list-a/o?".len() + fixed_query.len();
    for length in [usize::from(u16::MAX) - 1, usize::from(u16::MAX)] {
        let request = pb::ListObjectsRequest {
            bucket: "list-a".into(),
            prefix: "a".repeat(length - fixed_uri_len),
            ..Default::default()
        };
        let uri = format!("/storage/v1/b/list-a/o?prefix={}&delimiter=&pageToken=&maxResults=1000&versions=false&includeTrailingDelimiter=false", request.prefix).parse::<hyper::Uri>();
        let error = assert_code(
            storage.list_objects(Request::new(request)).await,
            tonic::Code::InvalidArgument,
        );
        assert_eq!(
            error.message(),
            if uri.is_ok() {
                "Invalid object listing or page token; use the token with its original query"
            } else {
                "Invalid list request"
            }
        );
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn object_listing_preserves_json_pagination_prefixes_versions_and_errors() {
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    for name in [
        "pref%ix/a b",
        "pref%ix/a b",
        "pref%ix/b/nested",
        "pref%ix/z",
    ] {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(Ok(crate::object::UploadChunk::End)).await.unwrap();
        drop(tx);
        storage
            .state
            .catalog
            .put_object(
                crate::storage::test_fixtures::permissive_upload("list-a", name, 1024),
                rx,
            )
            .await
            .unwrap();
    }
    for (versions, delimiter, trailing) in
        [(false, "", false), (true, "/", false), (false, "/", true)]
    {
        let mut request = pb::ListObjectsRequest {
            bucket: "list-a".into(),
            prefix: "pref%ix/".into(),
            delimiter: delimiter.into(),
            max_results: 1,
            versions,
            include_trailing_delimiter: trailing,
            ..Default::default()
        };
        loop {
            let encode = crate::auth::signature::encode_query_component;
            let uri = format!("/storage/v1/b/list-a/o?prefix={}&delimiter={}&pageToken={}&maxResults=1&versions={versions}&includeTrailingDelimiter={trailing}", encode(&request.prefix), encode(delimiter), encode(&request.page_token)).parse().unwrap();
            let Some(Ok(json::ObjectRequest::List(params))) =
                json::route_object_with_profile(&hyper::Method::GET, &uri, storage.compat_profile)
            else {
                panic!("valid reference listing");
            };
            let page = storage
                .state
                .catalog
                .list_objects(params.query.clone(), params.max_results, PERMISSIVE)
                .await
                .unwrap();
            let rendered = json::object_list(&page, &params, &storage.state.api_base_url).unwrap();
            let actual = storage
                .list_objects(Request::new(request.clone()))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(
                actual.items,
                page.items
                    .iter()
                    .map(convert::object)
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
            );
            assert_eq!(actual.prefixes, page.prefixes);
            assert_eq!(
                actual.next_page_token,
                rendered["nextPageToken"].as_str().unwrap_or_default()
            );
            if actual.next_page_token.is_empty() {
                break;
            }
            let mut changed = request.clone();
            changed.prefix.push('x');
            changed.page_token = actual.next_page_token.clone();
            // A token is a position, so another prefix continues after it, as on Cloud Storage.
            let changed = storage
                .list_objects(Request::new(changed))
                .await
                .unwrap()
                .into_inner();
            assert!(changed.items.is_empty() && changed.prefixes.is_empty());
            request.page_token = actual.next_page_token;
        }
    }
    let error = assert_code(
        storage
            .list_objects(Request::new(pb::ListObjectsRequest {
                bucket: "list-a".into(),
                page_token: "!".into(),
                ..Default::default()
            }))
            .await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(
        error.message(),
        "Invalid object listing or page token; use the token with its original query"
    );
    let error = assert_code(
        storage
            .list_objects(Request::new(pb::ListObjectsRequest {
                bucket: "list-a".into(),
                max_results: -1,
                ..Default::default()
            }))
            .await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(
        error.message(),
        "max_results must be nonnegative, for example 100"
    );
    storage.state.catalog.close().await.unwrap();
}

/// The wire text of a failed bucket update or delete precondition; reads word it differently.
const PRECONDITION_NOT_HELD: &str =
    "At least one of the pre-conditions you specified did not hold.";

#[tokio::test]
async fn bucket_metageneration_conditions_gate_get_update_and_delete() {
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    for (matching, not_matching, succeeds) in [
        (Some(1), None, true),
        (Some(2), None, false),
        (None, Some(1), false),
        (None, Some(2), true),
    ] {
        let result = storage
            .get_bucket(Request::new(pb::GetBucketRequest {
                bucket: "list-a".into(),
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
                ..Default::default()
            }))
            .await;
        if succeeds {
            assert_eq!(result.unwrap().into_inner().metageneration, 1);
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.code(), tonic::Code::FailedPrecondition);
            assert_eq!(
                error.message(),
                "Bucket metageneration condition failed; read current metadata"
            );
        }
    }
    for (matching, not_matching) in [(Some(2), None), (None, Some(1))] {
        let error = storage
            .update_bucket(Request::new(pb::UpdateBucketRequest {
                bucket: "list-a".into(),
                metadata: Some(pb::Bucket::default()),
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(error.message(), PRECONDITION_NOT_HELD);
        assert_eq!(
            storage
                .state
                .catalog
                .get_bucket("list-a", PERMISSIVE)
                .await
                .unwrap()
                .unwrap()
                .metageneration,
            1
        );
    }
    let updated = storage
        .update_bucket(Request::new(pb::UpdateBucketRequest {
            bucket: "list-a".into(),
            metadata: Some(pb::Bucket::default()),
            if_metageneration_match: Some(1),
            if_metageneration_not_match: Some(2),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(updated.metageneration, 2);
    for (matching, not_matching) in [(Some(1), None), (None, Some(2))] {
        let error = storage
            .delete_bucket(Request::new(pb::DeleteBucketRequest {
                bucket: "list-a".into(),
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(error.message(), PRECONDITION_NOT_HELD);
        assert!(
            storage
                .state
                .catalog
                .get_bucket("list-a", PERMISSIVE)
                .await
                .unwrap()
                .is_some()
        );
    }
    storage
        .delete_bucket(Request::new(pb::DeleteBucketRequest {
            bucket: "list-a".into(),
            if_metageneration_match: Some(2),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(
        storage
            .state
            .catalog
            .get_bucket("list-a", PERMISSIVE)
            .await
            .unwrap()
            .is_none()
    );
    storage.state.catalog.close().await.unwrap();
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
    assert_code(
        storage
            .list_buckets(list("p1", None, 2, &first.next_page_token))
            .await,
        tonic::Code::InvalidArgument,
    );
    let rest = storage
        .list_buckets(list("", None, 2, &first.next_page_token))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&rest), ["list-c"]);
    assert!(rest.next_page_token.is_empty());
    let project_token = json::encode_page_token("p1", "list-", "list-a").unwrap();
    assert_code(
        storage
            .list_buckets(list("", None, 2, &project_token))
            .await,
        tonic::Code::InvalidArgument,
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
    assert_code(
        storage
            .list_buckets(list("", Some("tok-none"), 0, ""))
            .await,
        tonic::Code::PermissionDenied,
    );
    assert_code(
        storage.list_buckets(list("", None, 0, "")).await,
        tonic::Code::Unauthenticated,
    );
    assert_code(
        storage
            .list_buckets(list("p3", Some("tok-two"), 0, ""))
            .await,
        tonic::Code::PermissionDenied,
    );
    storage.state.catalog.close().await.unwrap();
}
