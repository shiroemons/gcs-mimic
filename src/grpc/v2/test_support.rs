//! Fixtures shared by the Storage v2 handler tests: a catalog with two buckets and uploads into it.

use super::{Storage, pb, routing::Admitted};
use crate::{
    auth::Auth,
    bucket::{BucketRecord, SoftDeletePolicy},
    object::UploadChunk,
    storage::{
        Catalog,
        test_fixtures::{PERMISSIVE, live, permissive_upload},
    },
    test_support::TestDir,
};
use std::sync::Arc;

pub(super) const RESOURCE: &str = "projects/_/buckets/update-copy-bucket";

/// The routing admission of a handler a test calls directly, without the dispatch: nothing was
/// admitted, so nothing is compared. Production builds have no such admission.
pub(super) const UNDISPATCHED: &Admitted = &Admitted::Undispatched;

/// A request for a handler a test calls directly, without the dispatch, carrying
/// [`UNDISPATCHED`] where the dispatch would store the call's routing admission.
pub(super) fn undispatched<T>(message: T) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    request.extensions_mut().insert(Admitted::Undispatched);
    request
}

pub(super) async fn setup() -> (Storage, TestDir) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let now = std::time::SystemTime::now();
    for (name, location) in [("update-copy-bucket", "US"), ("update-copy-other", "EU")] {
        let record = BucketRecord {
            location: location.parse().unwrap(),
            labels: [
                ("keep".into(), "yes".into()),
                ("remove".into(), "yes".into()),
            ]
            .into(),
            soft_delete_policy: Some(SoftDeletePolicy {
                retention_duration_seconds: 604800,
                effective_time: now,
            }),
            ..BucketRecord::for_test(name, "local-project", now)
        };
        catalog.insert_bucket(record).await.unwrap();
    }
    (
        Storage {
            compat_profile: crate::profile::CompatProfile::Native,
            state: Arc::new(crate::server::State::for_test(catalog, Auth::permissive())),
            max_object_bytes: 8 * 1024 * 1024,
        },
        dir,
    )
}

/// The second fixture bucket, as a resource name.
pub(super) const OTHER_RESOURCE: &str = "projects/_/buckets/update-copy-other";

/// Removes the soft delete policy of the second fixture bucket, so an object deleted there
/// leaves no soft-deleted generation.
pub(super) async fn without_soft_delete(storage: &Storage) {
    let update = crate::bucket::BucketMetadataUpdate {
        soft_delete_retention: Some(0_i64.try_into().unwrap()),
        ..Default::default()
    };
    storage
        .state
        .catalog
        .patch_bucket("update-copy-other", update, Default::default(), PERMISSIVE)
        .await
        .unwrap();
}

pub(super) async fn upload(
    storage: &Storage,
    name: &str,
    length: usize,
) -> crate::object::ObjectRecord {
    upload_to(storage, "update-copy-bucket", name, length).await
}

pub(super) async fn upload_to(
    storage: &Storage,
    bucket: &str,
    name: &str,
    length: usize,
) -> crate::object::ObjectRecord {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.send(Ok(UploadChunk::Data(bytes::Bytes::from(vec![42; length]))))
        .await
        .unwrap();
    tx.send(Ok(UploadChunk::End)).await.unwrap();
    drop(tx);
    let upload = permissive_upload(bucket, name, storage.max_object_bytes);
    storage.state.catalog.put_object(upload, rx).await.unwrap()
}

/// The live object, or `None` once it is absent. Any other failure is a broken fixture.
pub(super) async fn stored_object(s: &Storage, name: &str) -> Option<crate::object::ObjectRecord> {
    let target = live("update-copy-bucket", name);
    match s
        .state
        .catalog
        .get_object(target, Default::default(), PERMISSIVE)
        .await
    {
        Ok(record) => Some(record),
        Err(crate::object::ObjectError::ObjectNotFound) => None,
        Err(error) => panic!("{name}: {error:?}"),
    }
}

pub(super) fn grants(object: &pb::Object) -> Vec<(&str, &str)> {
    object
        .acl
        .iter()
        .map(|entry| (entry.entity.as_str(), entry.role.as_str()))
        .collect()
}
