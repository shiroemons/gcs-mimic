//! ListObjects paging, soft-deleted listing and folder prefixes.

use super::super::super::test_support::{setup, upload};
use super::*;
use crate::grpc::test_support::{assert_code, assert_unsupported, caller, registry};
use crate::storage::test_fixtures::{PERMISSIVE, permissive_upload};
use crate::{
    auth::Auth,
    object::{MapPatch, UploadChunk},
    storage::Catalog,
    test_support::TestDir,
};
use std::sync::Arc;
use tonic::Code;

/// A page must fit the 4 MiB that clients commonly accept in one message, however much the
/// objects hold, and paging must still reach every object.
#[tokio::test]
async fn grpc_object_lists_page_on_their_size_and_reach_every_object() {
    use prost::Message;
    const OBJECTS: usize = 600;
    let (s, _dir) = setup().await;
    for index in 0..OBJECTS {
        let name = format!("object-{index:04}");
        let mut upload = permissive_upload("update-copy-bucket", &name, s.max_object_bytes);
        upload.metadata.metadata = MapPatch::Merge(std::collections::BTreeMap::from([(
            "key".to_owned(),
            Some("v".repeat(8000)),
        )]));
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(Ok(UploadChunk::End)).await.unwrap();
        drop(tx);
        s.state.catalog.put_object(upload, rx).await.unwrap();
    }
    let (mut listed, mut pages, mut token) = (Vec::new(), 0, String::new());
    loop {
        let page = s
            .list_objects(
                Request::new(pb::ListObjectsRequest {
                    parent: "projects/_/buckets/update-copy-bucket".into(),
                    page_token: std::mem::take(&mut token),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(
            page.encoded_len() < 4 * 1024 * 1024,
            "{}",
            page.encoded_len()
        );
        assert!(!page.objects.is_empty(), "a page must advance");
        listed.extend(page.objects.into_iter().map(|object| object.name));
        pages += 1;
        if page.next_page_token.is_empty() {
            break;
        }
        token = page.next_page_token;
    }
    assert!(pages > 1, "{pages}");
    let expected: Vec<_> = (0..OBJECTS)
        .map(|index| format!("object-{index:04}"))
        .collect();
    assert_eq!(listed, expected);
    s.state.catalog.close().await.unwrap();
}

/// A budget smaller than any entry still lists one entry per page, prefixes included.
#[tokio::test]
async fn an_object_page_past_its_budget_still_advances_one_entry_at_a_time() {
    let (s, _dir) = setup().await;
    for name in ["dir/a", "dir/b", "top-1", "top-2"] {
        upload(&s, name, 1).await;
    }
    let mut query = crate::storage::ObjectListQuery {
        bucket: "update-copy-bucket".into(),
        delimiter: "/".into(),
        page_bytes: Some(1),
        ..Default::default()
    };
    let mut seen = Vec::new();
    loop {
        let page = s
            .state
            .catalog
            .list_objects(query.clone(), 1000, PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(page.items.len() + page.prefixes.len(), 1);
        seen.extend(page.prefixes);
        seen.extend(page.items.into_iter().map(|record| record.name));
        match page.next {
            Some(next) => query.cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(seen, ["dir/", "top-1", "top-2"]);
    s.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_list_soft_deleted_objects() {
    let (s, _dir) = setup().await;
    let resource = "projects/_/buckets/update-copy-bucket";
    let deleted = upload(&s, "gone", 3).await;
    upload(&s, "live", 3).await;
    s.delete_object(
        Request::new(pb::DeleteObjectRequest {
            bucket: resource.into(),
            object: "gone".into(),
            ..Default::default()
        }),
        None,
    )
    .await
    .unwrap();
    let list = |soft_deleted: bool, versions: bool| pb::ListObjectsRequest {
        parent: resource.into(),
        soft_deleted,
        versions,
        ..Default::default()
    };
    let live = s
        .list_objects(Request::new(list(false, false)), None)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        live.objects
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        ["live"]
    );
    let soft = s
        .list_objects(Request::new(list(true, false)), None)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(soft.objects.len(), 1);
    assert_eq!(soft.objects[0].name, "gone");
    assert_eq!(soft.objects[0].generation, deleted.generation.get());
    assert!(soft.objects[0].soft_delete_time.is_some());
    assert!(soft.objects[0].hard_delete_time.is_some());
    // The JSON listing rejects softDeleted with versions; gRPC shares that rule.
    assert_code(
        s.list_objects(Request::new(list(true, true)), None).await,
        Code::InvalidArgument,
    );
    s.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_list_folders_as_prefixes_follows_the_json_rule() {
    let (s, _dir) = setup().await;
    upload(&s, "top", 3).await;
    upload(&s, "dir/nested", 3).await;
    let list = |include_folders_as_prefixes: bool, delimiter: &str| pb::ListObjectsRequest {
        parent: "projects/_/buckets/update-copy-bucket".into(),
        delimiter: delimiter.into(),
        include_folders_as_prefixes,
        ..Default::default()
    };
    // No local bucket holds folders or managed folders, so the flag changes nothing.
    let plain = s
        .list_objects(Request::new(list(false, "/")), None)
        .await
        .unwrap()
        .into_inner();
    let folders = s
        .list_objects(Request::new(list(true, "/")), None)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(folders, plain);
    assert_eq!(folders.prefixes, ["dir/"]);
    assert_eq!(folders.objects.len(), 1);
    for delimiter in ["", "-"] {
        let error = s
            .list_objects(Request::new(list(true, delimiter)), None)
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::InvalidArgument, "{delimiter:?}");
        assert!(error.message().contains("delimiter=/"), "{error:?}");
    }
    assert_unsupported(
        s.list_objects(
            Request::new(pb::ListObjectsRequest {
                filter: "contexts.\"k\"=\"v\"".into(),
                ..list(false, "/")
            }),
            None,
        )
        .await,
        "ListObjects filter",
    );
    s.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_list_soft_deleted_objects_strict() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let now = std::time::SystemTime::now();
    let bucket = crate::bucket::BucketRecord {
        soft_delete_policy: Some(crate::bucket::SoftDeletePolicy {
            retention_duration_seconds: 604800,
            effective_time: now,
        }),
        ..crate::bucket::BucketRecord::for_test("update-copy-bucket", "p", now)
    };
    catalog.insert_bucket(bucket).await.unwrap();
    let s = Storage {
        compat_profile: crate::config::CompatProfile::Native,
        state: Arc::new(crate::server::State::for_test(
            catalog,
            Auth::strict(registry(&[
                ("viewer", "p", "roles/storage.objectViewer"),
                ("none", "", ""),
            ])),
        )),
        max_object_bytes: 1024,
    };
    let request = pb::ListObjectsRequest {
        parent: "projects/_/buckets/update-copy-bucket".into(),
        soft_deleted: true,
        ..Default::default()
    };
    s.list_objects(Request::new(request.clone()), caller(&s, "viewer").await)
        .await
        .unwrap();
    assert_code(
        s.list_objects(Request::new(request.clone()), caller(&s, "none").await)
            .await,
        Code::PermissionDenied,
    );
    assert_code(
        s.list_objects(Request::new(request), Some(Caller::Anonymous))
            .await,
        Code::Unauthenticated,
    );
    s.state.catalog.close().await.unwrap();
}
