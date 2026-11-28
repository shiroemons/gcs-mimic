use super::pb::storage_server::Storage as Legacy;
use super::*;
use crate::grpc::test_support::{self as fixtures, assert_code};
use crate::{
    auth::{Auth, Registry},
    bucket::BucketRecord,
    json,
    storage::{Catalog, test_fixtures::PERMISSIVE},
    test_support::TestDir,
};
use std::time::{Duration, UNIX_EPOCH};

/// Catalog failures that reach the legacy bucket handlers through a distinct path.
pub(in crate::grpc) fn bucket_golden_status(
    error: CatalogError,
    caller: Option<Caller<'_>>,
) -> Option<Status> {
    if matches!(error, CatalogError::NotModified) {
        Some(conditional_bucket_error(error))
    } else if matches!(
        error,
        CatalogError::Denied(_)
            | CatalogError::Forbidden
            | CatalogError::Unavailable(_)
            | CatalogError::Io(_)
            | CatalogError::Corrupt(_)
    ) {
        Some(access_status(error, caller))
    } else {
        None
    }
}

#[test]
fn typed_object_metadata_keeps_replace_resets_and_patch_omissions() {
    for mode in [json::WriteMode::Patch, json::WriteMode::Replace] {
        let update = metadata(pb::Object::default(), mode).unwrap();
        assert_eq!(update.write, mode);
        assert_eq!(
            update.content_type,
            if mode == json::WriteMode::Replace {
                crate::object::Patch::Clear
            } else {
                crate::object::Patch::Keep
            }
        );
        assert_eq!(
            update.metadata,
            if mode == json::WriteMode::Replace {
                crate::object::MapPatch::Merge(Default::default())
            } else {
                crate::object::MapPatch::Keep
            }
        );
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

/// Legacy writes share the JSON bound on encoded metadata instead of an 8 KiB cap: Cloud Storage
/// stored custom metadata past its documented 8 KiB through JSON, XML and Storage v2 (2026-11-15).
#[test]
fn typed_object_metadata_takes_the_shared_bound_on_encoded_metadata() {
    let object = |length: usize| pb::Object {
        metadata: [("k".into(), "v".repeat(length))].into(),
        ..Default::default()
    };
    let update = metadata(object(40_000), json::WriteMode::Patch).unwrap();
    let crate::object::MapPatch::Merge(entries) = update.metadata else {
        panic!("custom metadata is merged");
    };
    assert_eq!(entries["k"].as_deref().map(str::len), Some(40_000));
    let refused = metadata(object(41_000), json::WriteMode::Patch).unwrap_err();
    assert_eq!(refused.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        refused.message(),
        "Object metadata exceeds 40 KiB once encoded as JSON; shorten its fixed and custom metadata."
    );
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
        state: Arc::new(crate::service::state::State::for_test(catalog, auth)),
        max_object_bytes: 1024 * 1024,
        compat_profile: crate::profile::CompatProfile::FakeGcsServer,
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
        Legacy::delete_bucket(
            &storage,
            Request::new(pb::DeleteBucketRequest {
                bucket: "list-a".into(),
                ..Default::default()
            }),
        )
        .await,
        tonic::Code::FailedPrecondition,
    );
    assert_eq!(
        legacy.message(),
        "The bucket you tried to delete is not empty."
    );
    let modern = fixtures::storage_with(storage.state.clone(), 1024);
    let modern = assert_code(
        V2Storage::delete_bucket(
            &modern,
            fixtures::routed_request(
                &fixtures::bucket_routing("list-a"),
                v2_pb::DeleteBucketRequest {
                    name: "projects/_/buckets/list-a".into(),
                    ..Default::default()
                },
            ),
        )
        .await,
        tonic::Code::FailedPrecondition,
    );
    assert_eq!(
        modern.message(),
        "The bucket you tried to delete is not empty."
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn object_listing_preserves_encoded_query_and_uri_limits() {
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    let fixed_query = "prefix=&delimiter=&pageToken=&maxResults=1000&versions=false&includeTrailingDelimiter=false";
    for (prefix, expected) in [
        (
            "a".repeat(crate::percent::MAX_QUERY_BYTES - fixed_query.len()),
            None,
        ),
        (
            "a".repeat(crate::percent::MAX_QUERY_BYTES - fixed_query.len() + 1),
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
            None => {
                assert!(crate::percent::parse_query(uri.as_ref().unwrap().query().unwrap()).is_ok())
            }
            Some("Invalid list request") => assert!(uri.is_err()),
            Some(_) => assert!(
                crate::percent::parse_query(uri.as_ref().unwrap().query().unwrap()).is_err()
            ),
        }
        let actual = Legacy::list_objects(
            &storage,
            Request::new(pb::ListObjectsRequest {
                bucket: "list-a".into(),
                prefix,
                ..Default::default()
            }),
        )
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
            Legacy::list_objects(&storage, Request::new(request)).await,
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
            let actual = Legacy::list_objects(&storage, Request::new(request.clone()))
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
            let changed = Legacy::list_objects(&storage, Request::new(changed))
                .await
                .unwrap()
                .into_inner();
            assert!(changed.items.is_empty() && changed.prefixes.is_empty());
            request.page_token = actual.next_page_token;
        }
    }
    let error = assert_code(
        Legacy::list_objects(
            &storage,
            Request::new(pb::ListObjectsRequest {
                bucket: "list-a".into(),
                page_token: "!".into(),
                ..Default::default()
            }),
        )
        .await,
        tonic::Code::InvalidArgument,
    );
    assert_eq!(
        error.message(),
        "Invalid object listing or page token; use the token with its original query"
    );
    let error = assert_code(
        Legacy::list_objects(
            &storage,
            Request::new(pb::ListObjectsRequest {
                bucket: "list-a".into(),
                max_results: -1,
                ..Default::default()
            }),
        )
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

/// Storage v2 words a not-match that alone fails as a resource that has not been modified; the
/// legacy DeleteBucket and UpdateBucket keep the failed-condition answer, details included,
/// for both catalog failures.
#[test]
fn legacy_bucket_conditions_keep_the_failed_condition_answer() {
    for error in [CatalogError::NotModified, CatalogError::PreconditionFailed] {
        let status = conditional_bucket_error(error);
        assert_eq!(status.code(), tonic::Code::FailedPrecondition);
        assert_eq!(status.message(), PRECONDITION_NOT_HELD);
        assert!(status.details().is_empty());
    }
    assert_ne!(
        Status::from(CatalogError::NotModified).message(),
        PRECONDITION_NOT_HELD
    );
    let other = conditional_bucket_error(CatalogError::NotEmpty);
    assert_eq!(
        other.message(),
        Status::from(CatalogError::NotEmpty).message()
    );
}

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
        let result = Legacy::get_bucket(
            &storage,
            Request::new(pb::GetBucketRequest {
                bucket: "list-a".into(),
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
                ..Default::default()
            }),
        )
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
        let error = Legacy::update_bucket(
            &storage,
            Request::new(pb::UpdateBucketRequest {
                bucket: "list-a".into(),
                metadata: Some(pb::Bucket::default()),
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
                ..Default::default()
            }),
        )
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
    let updated = Legacy::update_bucket(
        &storage,
        Request::new(pb::UpdateBucketRequest {
            bucket: "list-a".into(),
            metadata: Some(pb::Bucket::default()),
            if_metageneration_match: Some(1),
            if_metageneration_not_match: Some(2),
            ..Default::default()
        }),
    )
    .await
    .unwrap()
    .into_inner();
    assert_eq!(updated.metageneration, 2);
    for (matching, not_matching) in [(Some(1), None), (None, Some(2))] {
        let error = Legacy::delete_bucket(
            &storage,
            Request::new(pb::DeleteBucketRequest {
                bucket: "list-a".into(),
                if_metageneration_match: matching,
                if_metageneration_not_match: not_matching,
                ..Default::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(error.message(), PRECONDITION_NOT_HELD);
        assert!(error.details().is_empty());
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
    Legacy::delete_bucket(
        &storage,
        Request::new(pb::DeleteBucketRequest {
            bucket: "list-a".into(),
            if_metageneration_match: Some(2),
            ..Default::default()
        }),
    )
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

/// A legacy bucket carries the number JSON and Storage v2 render for its project, which names
/// the project in a listing; an update may send it back as fetched but may not change it.
#[tokio::test]
async fn a_legacy_bucket_carries_its_project_number() {
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    let number = crate::project::ProjectNumber::of("p2");
    let fetched = Legacy::get_bucket(
        &storage,
        Request::new(pb::GetBucketRequest {
            bucket: "list-a".into(),
            ..Default::default()
        }),
    )
    .await
    .unwrap()
    .into_inner();
    assert_eq!(fetched.project_number, i64::try_from(number).unwrap());
    let listed = Legacy::list_buckets(&storage, list(&number.to_string(), None, 0, ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&listed), ["list-a"]);
    assert_eq!(listed.items[0].project_number, fetched.project_number);
    let update = |metadata: pb::Bucket| {
        Legacy::update_bucket(
            &storage,
            Request::new(pb::UpdateBucketRequest {
                bucket: "list-a".into(),
                metadata: Some(metadata),
                ..Default::default()
            }),
        )
    };
    let changed = pb::Bucket {
        project_number: fetched.project_number + 1,
        ..fetched.clone()
    };
    let refused = update(changed).await.unwrap_err();
    assert!(refused.message().contains("project_number"), "{refused:?}");
    let updated = update(fetched.clone()).await.unwrap().into_inner();
    assert_eq!(updated.metageneration, fetched.metageneration + 1);
    assert_eq!(updated.project_number, fetched.project_number);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn list_buckets_without_project_spans_projects_with_paging() {
    let dir = TestDir::new();
    let storage = storage(&dir, Auth::permissive()).await;
    let first = Legacy::list_buckets(&storage, list("", None, 2, ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&first), ["list-a", "list-b"]);
    assert!(!first.next_page_token.is_empty());
    // An all-projects cursor is not a project cursor, and vice versa.
    assert_code(
        Legacy::list_buckets(&storage, list("p1", None, 2, &first.next_page_token)).await,
        tonic::Code::InvalidArgument,
    );
    let rest = Legacy::list_buckets(&storage, list("", None, 2, &first.next_page_token))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&rest), ["list-c"]);
    assert!(rest.next_page_token.is_empty());
    let project_token = crate::page_token::encode_page_token("p1", "list-", "list-a").unwrap();
    assert_code(
        Legacy::list_buckets(&storage, list("", None, 2, &project_token)).await,
        tonic::Code::InvalidArgument,
    );
    let scoped = Legacy::list_buckets(&storage, list("p1", None, 0, ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&scoped), ["list-b"]);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn anonymous_bucket_access_names_the_refused_permission() {
    let dir = TestDir::new();
    let registry = Registry::parse(br#"{"identities":[{"id":"reader","tokens":[]}]}"#).unwrap();
    let storage = storage(&dir, Auth::strict(registry)).await;
    let status = Legacy::get_bucket(
        &storage,
        Request::new(pb::GetBucketRequest {
            bucket: "list-a".into(),
            ..Default::default()
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
    assert_eq!(
        status.message(),
        "Anonymous caller does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist)."
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn authenticated_bucket_access_names_the_principal_and_refused_permission() {
    let dir = TestDir::new();
    let registry =
        Registry::parse(br#"{"identities":[{"id":"none@example.invalid","tokens":["tok-none"]}]}"#)
            .unwrap();
    let storage = storage(&dir, Auth::strict(registry)).await;
    let mut request = Request::new(pb::GetBucketRequest {
        bucket: "list-a".into(),
        ..Default::default()
    });
    request
        .metadata_mut()
        .insert("authorization", "Bearer tok-none".parse().unwrap());
    let status = Legacy::get_bucket(&storage, request).await.unwrap_err();
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    assert_eq!(
        status.message(),
        "none@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist)."
    );
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
    let first = Legacy::list_buckets(&storage, list("", Some("tok-two"), 1, ""))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&first), ["list-a"]);
    let rest = Legacy::list_buckets(
        &storage,
        list("", Some("tok-two"), 1000, &first.next_page_token),
    )
    .await
    .unwrap()
    .into_inner();
    // objectViewer lacks storage.buckets.list, so the p3 bucket stays hidden.
    assert_eq!(names(&rest), ["list-b"]);
    assert!(rest.next_page_token.is_empty());
    assert_code(
        Legacy::list_buckets(&storage, list("", Some("tok-none"), 0, "")).await,
        tonic::Code::PermissionDenied,
    );
    assert_code(
        Legacy::list_buckets(&storage, list("", None, 0, "")).await,
        tonic::Code::Unauthenticated,
    );
    assert_code(
        Legacy::list_buckets(&storage, list("p3", Some("tok-two"), 0, "")).await,
        tonic::Code::PermissionDenied,
    );
    storage.state.catalog.close().await.unwrap();
}

/// Both legacy update forms release a held event-based hold, a patch by sending false and an
/// update by leaving it out, and either starts the bucket retention period again. The deadline
/// is absent while the hold is held, as the v1 `retention_expiration_time` comment states.
#[tokio::test]
async fn legacy_updates_releasing_the_event_based_hold_restart_the_retention_period() {
    let seconds = |seconds: u64| UNIX_EPOCH + Duration::from_secs(seconds);
    let expiration = |seconds: i64| Some(prost_types::Timestamp { seconds, nanos: 0 });
    let dir = TestDir::new();
    let clock = Arc::new(crate::clock::ManualClock::new(seconds(1000)));
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    let mut record = bucket("held", "p1");
    record.retention_policy = Some(crate::bucket::RetentionPolicy {
        retention_period: 10,
        effective_time: seconds(1000),
        is_locked: false,
    });
    catalog.insert_bucket(record).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(Ok(crate::object::UploadChunk::End)).await.unwrap();
    drop(tx);
    let mut upload = crate::storage::test_fixtures::permissive_upload("held", "object", 1024);
    upload.metadata.event_based_hold = Some(true);
    catalog.put_object(upload, rx).await.unwrap();
    let storage = Storage {
        state: Arc::new(crate::service::state::State::for_test(
            catalog,
            Auth::permissive(),
        )),
        max_object_bytes: 1024,
        compat_profile: crate::profile::CompatProfile::FakeGcsServer,
    };
    let patch = |held: bool| {
        Request::new(pb::PatchObjectRequest {
            bucket: "held".into(),
            object: "object".into(),
            metadata: Some(pb::Object {
                event_based_hold: Some(held),
                ..Default::default()
            }),
            ..Default::default()
        })
    };
    clock.set(seconds(2000));
    let released = Legacy::patch_object(&storage, patch(false)).await.unwrap();
    assert_eq!(
        released.get_ref().retention_expiration_time,
        expiration(2010)
    );
    let held = Legacy::patch_object(&storage, patch(true)).await.unwrap();
    assert_eq!(held.get_ref().retention_expiration_time, None);
    clock.set(seconds(3000));
    let replaced = Legacy::update_object(
        &storage,
        Request::new(pb::UpdateObjectRequest {
            bucket: "held".into(),
            object: "object".into(),
            metadata: Some(pb::Object::default()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        replaced.get_ref().retention_expiration_time,
        expiration(3010)
    );
    storage.state.catalog.close().await.unwrap();
}
