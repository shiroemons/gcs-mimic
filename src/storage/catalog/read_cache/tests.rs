use super::super::sql;
use super::*;
use crate::{
    auth::Binding,
    bucket::BucketMetadataUpdate,
    object::{ObjectError, ObjectMetadataUpdate, Patch, ReadAhead},
    storage::test_fixtures::{PERMISSIVE, body, bucket, live, permissive_upload},
    test_support::TestDir,
    time_limit::TimeLimit,
};
use bytes::Bytes;
use std::{fs::File, sync::atomic::AtomicI64};

async fn open_catalog(dir: &TestDir) -> Catalog {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    catalog
}

async fn put(catalog: &Catalog, name: &str, contents: impl Into<Bytes>) -> ObjectRecord {
    catalog
        .put_object(
            permissive_upload("bucket", name, 8 << 20),
            body(vec![contents.into()]),
        )
        .await
        .unwrap()
}

async fn open(
    catalog: &Catalog,
    name: &str,
    read_ahead: ReadAhead,
) -> Result<(ObjectRecord, crate::storage::ObjectReader), ObjectError> {
    catalog
        .open_object_keyed(
            live("bucket", name),
            Default::default(),
            PERMISSIVE,
            None,
            read_ahead,
        )
        .await
}

async fn download(catalog: &Catalog, name: &str) -> Result<(ObjectRecord, Vec<u8>), ObjectError> {
    let (record, mut reader) = open(catalog, name, ReadAhead::Body).await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&chunk);
    }
    Ok((record, bytes))
}

async fn metadata(catalog: &Catalog, name: &str) -> Result<ObjectRecord, ObjectError> {
    catalog
        .get_object(live("bucket", name), Default::default(), PERMISSIVE)
        .await
}

async fn keeps_file(catalog: &Catalog, name: &str) -> bool {
    catalog
        .cached_object(&live("bucket", name))
        .await
        .is_some_and(|object| object.download.is_some())
}

#[tokio::test]
async fn a_repeated_read_does_not_reach_the_catalog() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    let written = put(&catalog, "object", &b"contents"[..]).await;
    assert_eq!(metadata(&catalog, "object").await.unwrap(), written);
    let served = catalog.pooled_reads();
    // Nothing behind the cache can answer while every path to SQLite is held.
    let admission = catalog.hold_admission();
    assert_eq!(metadata(&catalog, "object").await.unwrap(), written);
    drop(admission);
    assert_eq!(catalog.pooled_reads(), served);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_repeated_download_does_not_reach_the_catalog() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    assert!(!keeps_file(&catalog, "object").await);
    download(&catalog, "object").await.unwrap();
    assert!(keeps_file(&catalog, "object").await);
    let served = catalog.pooled_reads();
    let admission = catalog.hold_admission();
    let (_, bytes) = download(&catalog, "object").await.unwrap();
    drop(admission);
    assert_eq!(bytes, b"contents");
    assert_eq!(catalog.pooled_reads(), served);
    catalog.close().await.unwrap();
}

async fn open_ram_catalog(max_bytes: u64) -> Catalog {
    let catalog = Catalog::open_memory_for_server(
        max_bytes,
        1 << 20,
        Arc::new(crate::clock::SystemClock),
        super::super::admin::LocalAdmin::Disabled,
    )
    .await
    .unwrap();
    catalog
        .set_principal_profile(super::super::principals::Profile::TrustedLocal)
        .unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    catalog
}

#[tokio::test]
async fn a_repeated_ram_read_does_not_reach_the_catalog() {
    let catalog = open_ram_catalog(1 << 20).await;
    let written = put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    assert!(keeps_file(&catalog, "object").await);
    let admission = catalog.hold_admission();
    assert_eq!(metadata(&catalog, "object").await.unwrap(), written);
    let (_, bytes) = download(&catalog, "object").await.unwrap();
    drop(admission);
    assert_eq!(bytes, b"contents");
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_ram_write_reaches_the_very_next_read() {
    let catalog = open_ram_catalog(1 << 20).await;
    put(&catalog, "object", &b"first"[..]).await;
    download(&catalog, "object").await.unwrap();

    let overwritten = put(&catalog, "object", &b"second"[..]).await;
    let (record, bytes) = download(&catalog, "object").await.unwrap();
    assert_eq!(record.generation, overwritten.generation);
    assert_eq!(bytes, b"second");

    catalog
        .delete_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(
        download(&catalog, "object").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    catalog.close().await.unwrap();
}

/// The entry shares the object's chunks, so a delete must clear it or the bytes stay charged.
#[tokio::test]
async fn a_cached_ram_body_leaves_the_budget_with_its_delete() {
    const BODY: usize = 512 * 1024;
    let catalog = open_ram_catalog(768 * 1024).await;
    put(&catalog, "first", vec![1; BODY]).await;
    download(&catalog, "first").await.unwrap();
    assert!(keeps_file(&catalog, "first").await);
    let refused = catalog
        .put_object(
            permissive_upload("bucket", "second", 8 << 20),
            body(vec![Bytes::from(vec![2; BODY])]),
        )
        .await;
    assert!(refused.is_err(), "the budget fits one body, not two");

    catalog
        .delete_object(live("bucket", "first"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    put(&catalog, "second", vec![2; BODY]).await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_ram_body_takes_no_descriptor_slot() {
    let catalog = open_ram_catalog(1 << 20).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    assert!(keeps_file(&catalog, "object").await);
    assert_eq!(catalog.0.read_cache.read().open_files, 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_ranged_ram_hit_leaves_the_cached_body_at_its_start() {
    let catalog = open_ram_catalog(1 << 20).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    let (_, mut reader) = open(&catalog, "object", ReadAhead::None).await.unwrap();
    reader.set_range(3, 2).await.unwrap();
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), &b"te"[..]);
    let (_, bytes) = download(&catalog, "object").await.unwrap();
    assert_eq!(bytes, b"contents");
    catalog.close().await.unwrap();
}

/// A record cached by a metadata read has no open file yet, and a download then fetches one.
#[tokio::test]
async fn a_download_after_a_metadata_read_keeps_the_file() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    metadata(&catalog, "object").await.unwrap();
    assert!(!keeps_file(&catalog, "object").await);
    let (_, bytes) = download(&catalog, "object").await.unwrap();
    assert_eq!(bytes, b"contents");
    assert!(keeps_file(&catalog, "object").await);
    metadata(&catalog, "object").await.unwrap();
    assert!(keeps_file(&catalog, "object").await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn every_kind_of_write_reaches_the_very_next_read() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"first"[..]).await;
    download(&catalog, "object").await.unwrap();

    let overwritten = put(&catalog, "object", &b"second"[..]).await;
    let (record, bytes) = download(&catalog, "object").await.unwrap();
    assert_eq!(record.generation, overwritten.generation);
    assert_eq!(bytes, b"second");

    let patched = catalog
        .update_object_metadata(
            live("bucket", "object"),
            Default::default(),
            ObjectMetadataUpdate::default(),
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(
        metadata(&catalog, "object").await.unwrap().metageneration,
        patched.metageneration
    );

    download(&catalog, "object").await.unwrap();
    catalog
        .delete_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(
        metadata(&catalog, "object").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    assert_eq!(
        download(&catalog, "object").await.err(),
        Some(ObjectError::ObjectNotFound)
    );
    catalog.close().await.unwrap();
}

/// The cached answer includes whether the object is public, which decides its caching headers.
#[tokio::test]
async fn a_policy_change_reaches_the_very_next_read() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    let (_, reader) = open(&catalog, "object", ReadAhead::Body).await.unwrap();
    assert!(!reader.publicly_readable);
    let mut policy = catalog.get_policy("bucket", PERMISSIVE).await.unwrap();
    policy.bindings.push(Binding {
        role: "roles/storage.objectViewer".into(),
        members: vec!["allUsers".into()],
    });
    catalog
        .set_policy("bucket", policy, PERMISSIVE)
        .await
        .unwrap();
    let (_, reader) = open(&catalog, "object", ReadAhead::Body).await.unwrap();
    assert!(reader.publicly_readable);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn deleting_a_bucket_reaches_the_very_next_read() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    assert!(!catalog.bucket_has_cors("bucket").await.unwrap());
    catalog
        .delete_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog
        .delete_bucket("bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(
        metadata(&catalog, "object").await.err(),
        Some(ObjectError::BucketNotFound)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_cors_change_reaches_the_very_next_read() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    assert!(!catalog.bucket_has_cors("bucket").await.unwrap());
    assert!(!catalog.bucket_has_cors("bucket").await.unwrap());
    catalog
        .patch_bucket(
            "bucket",
            BucketMetadataUpdate {
                cors: Some(vec![Default::default()]),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert!(catalog.bucket_has_cors("bucket").await.unwrap());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_hit_still_answers_the_read_conditions() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    let written = put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    let unmet = crate::object::ObjectConditions {
        if_generation_match: Some(written.generation + 1),
        ..Default::default()
    };
    let not_modified = crate::object::ObjectConditions {
        if_generation_not_match: Some(written.generation),
        ..Default::default()
    };
    let target = || live("bucket", "object");
    assert_eq!(
        catalog.get_object(target(), unmet, PERMISSIVE).await.err(),
        Some(ObjectError::PreconditionFailed)
    );
    assert_eq!(
        catalog.open_object(target(), unmet, PERMISSIVE).await.err(),
        Some(ObjectError::PreconditionFailed)
    );
    assert_eq!(
        catalog
            .get_object(target(), not_modified, PERMISSIVE)
            .await
            .err(),
        Some(ObjectError::NotModified)
    );
    assert_eq!(
        catalog
            .open_object(target(), not_modified, PERMISSIVE)
            .await
            .err(),
        Some(ObjectError::NotModified)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn only_a_live_read_with_permissive_access_shares_an_answer() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    let written = put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    let denied = BucketAccess::Policy {
        identity: None,
        permission: crate::auth::Permission::ObjectsGet,
        project: None,
    };
    assert_eq!(
        catalog
            .get_object(live("bucket", "object"), Default::default(), &denied)
            .await
            .err(),
        Some(ObjectError::Denied(denied.denied("bucket")))
    );
    assert_eq!(
        catalog
            .open_object(live("bucket", "object"), Default::default(), &denied)
            .await
            .err(),
        Some(ObjectError::Denied(denied.denied("bucket")))
    );
    let served = catalog.pooled_reads();
    let pinned = crate::storage::test_fixtures::version("bucket", "object", written.generation);
    catalog
        .get_object(pinned, Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(catalog.pooled_reads(), served + 1);
    catalog.close().await.unwrap();
}

/// A hit must not be a way around the check of the key that guards these bytes.
#[tokio::test]
async fn an_object_under_a_customer_key_never_enters() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    let sha256 = crate::csek::key_sha256(&[5; 32]);
    let key = crate::csek::CustomerKey::from_sha256(sha256);
    let mut upload = permissive_upload("bucket", "secret", 1 << 20);
    upload.metadata.customer_key_sha256 = Some(sha256);
    catalog
        .put_object(upload, body(vec![Bytes::from_static(b"secret")]))
        .await
        .unwrap();
    for _ in 0..3 {
        catalog
            .open_object_keyed(
                live("bucket", "secret"),
                Default::default(),
                PERMISSIVE,
                Some(key),
                ReadAhead::Body,
            )
            .await
            .unwrap();
    }
    assert!(
        catalog
            .cached_object(&live("bucket", "secret"))
            .await
            .is_none()
    );
    assert!(
        catalog
            .open_object(live("bucket", "secret"), Default::default(), PERMISSIVE)
            .await
            .is_err()
    );
    catalog.close().await.unwrap();
}

/// A stored-gzip response is chunked or transcoded according to its bytes, so a hit must leave it
/// to the streaming path just as a miss does.
#[tokio::test]
async fn a_stored_gzip_body_is_never_read_ahead() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    let mut upload = permissive_upload("bucket", "object", 1 << 20);
    upload.metadata.content_encoding = Patch::Set("gzip".into());
    catalog
        .put_object(upload, body(vec![Bytes::from_static(b"\x1f\x8b plain")]))
        .await
        .unwrap();
    for _ in 0..3 {
        let (_, reader) = open(&catalog, "object", ReadAhead::Body).await.unwrap();
        assert_eq!(reader.read_ahead_len(), None);
    }
    assert!(keeps_file(&catalog, "object").await);
    catalog.close().await.unwrap();
}

/// A truncated blob must keep failing where the client can see it, not pass as a smaller object.
#[tokio::test]
async fn a_short_blob_fails_on_a_hit_as_on_a_miss() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    let blob = catalog
        .run(|connection| {
            let stored = super::super::objects::lookup(connection, "bucket", "object")?;
            Ok::<_, ObjectError>(stored.unwrap().blob)
        })
        .await
        .unwrap();
    std::fs::write(blob.blob_path(&dir.0), b"short").unwrap();
    for _ in 0..3 {
        let (_, mut reader) = open(&catalog, "object", ReadAhead::Body).await.unwrap();
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"short"[..]);
        assert_eq!(
            reader.next_chunk().await.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
    catalog.close().await.unwrap();
}

/// A reader already holding the old generation keeps its bytes, as any open file does.
#[tokio::test]
async fn a_reader_keeps_its_generation_across_an_overwrite() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"first"[..]).await;
    download(&catalog, "object").await.unwrap();
    let (record, mut reader) = open(&catalog, "object", ReadAhead::None).await.unwrap();
    assert_eq!(record.size, 5);
    put(&catalog, "object", &b"second"[..]).await;
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"first"[..]);
    let (_, bytes) = download(&catalog, "object").await.unwrap();
    assert_eq!(bytes, b"second");
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_hit_serves_ranges_and_bounded_chunks() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    let contents: Vec<u8> = (0..200_000).map(|index| (index % 251) as u8).collect();
    put(&catalog, "object", contents.clone()).await;
    download(&catalog, "object").await.unwrap();
    assert!(keeps_file(&catalog, "object").await);
    let (_, mut reader) = open(&catalog, "object", ReadAhead::None).await.unwrap();
    let mut streamed = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        assert!(chunk.len() <= 64 * 1024);
        streamed.extend_from_slice(&chunk);
    }
    assert_eq!(streamed, contents);
    let (_, mut reader) = open(&catalog, "object", ReadAhead::None).await.unwrap();
    reader.set_range(70_000, 100_000).await.unwrap();
    assert_eq!(
        reader.peek_prefix(4).await.unwrap(),
        contents[70_000..70_004]
    );
    let mut ranged = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        ranged.extend_from_slice(&chunk);
    }
    assert_eq!(ranged, contents[70_000..170_000]);
    let (_, mut reader) = open(&catalog, "object", ReadAhead::None).await.unwrap();
    reader.set_range(199_000, 2_000).await.unwrap();
    assert_eq!(reader.next_chunk().await.unwrap().unwrap().len(), 1_000);
    assert_eq!(
        reader.next_chunk().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
    catalog.close().await.unwrap();
}

fn cached(name: &str, file: Option<Arc<File>>) -> CachedObject {
    CachedObject {
        record: ObjectRecord {
            bucket: "bucket".to_owned(),
            name: name.to_owned(),
            generation: 1,
            metageneration: 1,
            owner: None,
            content_type: "image/jpeg".to_owned(),
            content_encoding: None,
            content_disposition: None,
            content_language: None,
            cache_control: None,
            metadata: Default::default(),
            size: 0,
            md5: [0; 16],
            md5_availability: Default::default(),
            crc32c: 0,
            component_count: None,
            storage_class: crate::bucket::StorageClass::Standard,
            time_created: std::time::UNIX_EPOCH,
            time_storage_class_updated: None,
            updated: std::time::UNIX_EPOCH,
            time_deleted: None,
            soft_delete_time: None,
            hard_delete_time: None,
            event_based_hold: crate::object::Hold::Unset,
            temporary_hold: crate::object::Hold::Unset,
            custom_time: None,
            retention_expiration_time: None,
            retention: None,
            customer_key_sha256: None,
        },
        download: file.map(|file| CachedDownload {
            data: ReaderData::Disk(file),
            publicly_readable: false,
        }),
    }
}

/// Any open file does: the cache only counts them.
fn open_file(dir: &TestDir) -> Arc<File> {
    let path = dir.0.join("blob");
    std::fs::write(&path, b"contents").unwrap();
    Arc::new(File::open(path).unwrap())
}

/// Bounds the descriptors the cache can take from connections. A record keeps its use without one.
#[test]
fn past_the_descriptor_limit_only_the_record_enters() {
    let dir = TestDir::new();
    let cache = ReadCache::new(1);
    let revision = cache.revision();
    cache.store_object(revision, cached("first", Some(open_file(&dir))));
    cache.store_object(revision, cached("second", Some(open_file(&dir))));
    let contents = cache.read();
    assert_eq!(contents.open_files, 1);
    assert_eq!(contents.object_count, 2);
    assert!(
        contents
            .object("bucket", "first")
            .unwrap()
            .download
            .is_some()
    );
    assert!(
        contents
            .object("bucket", "second")
            .unwrap()
            .download
            .is_none()
    );
}

#[test]
fn a_limit_of_zero_descriptors_keeps_no_file_open() {
    let dir = TestDir::new();
    let cache = ReadCache::new(0);
    cache.store_object(cache.revision(), cached("name", Some(open_file(&dir))));
    let contents = cache.read();
    assert_eq!(contents.open_files, 0);
    assert!(
        contents
            .object("bucket", "name")
            .unwrap()
            .download
            .is_none()
    );
}

/// A sweep over more objects than fit keeps the ones that entered first, instead of replacing
/// each one just before the sweep returns to it.
#[test]
fn a_full_cache_keeps_what_it_holds() {
    let cache = ReadCache::new(MAX_OPEN_FILES);
    let revision = cache.revision();
    for index in 0..=MAX_OBJECTS {
        cache.store_object(revision, cached(&index.to_string(), None));
    }
    let contents = cache.read();
    assert_eq!(contents.object_count, MAX_OBJECTS);
    assert!(contents.object("bucket", "0").is_some());
    assert!(
        contents
            .object("bucket", &MAX_OBJECTS.to_string())
            .is_none()
    );
}

/// One record's custom metadata can be far larger than another's, so a count alone bounds nothing.
#[test]
fn the_record_bytes_are_bounded() {
    let cache = ReadCache::new(MAX_OPEN_FILES);
    let revision = cache.revision();
    let mut large = cached("large", None);
    large
        .record
        .metadata
        .insert("key".into(), "v".repeat(MAX_RECORD_BYTES + 1));
    cache.store_object(revision, large);
    cache.store_object(revision, cached("small", None));
    let contents = cache.read();
    assert!(contents.object("bucket", "large").is_none());
    assert!(contents.object("bucket", "small").is_some());
    assert!(contents.charged <= MAX_RECORD_BYTES);
}

#[test]
fn a_record_read_without_a_file_keeps_the_open_one() {
    let dir = TestDir::new();
    let cache = ReadCache::new(MAX_OPEN_FILES);
    let revision = cache.revision();
    cache.store_object(revision, cached("name", Some(open_file(&dir))));
    let charged = cache.read().charged;
    cache.store_object(revision, cached("name", None));
    let contents = cache.read();
    assert!(
        contents
            .object("bucket", "name")
            .unwrap()
            .download
            .is_some()
    );
    assert_eq!(contents.charged, charged);
    assert_eq!(contents.object_count, 1);
    assert_eq!(contents.open_files, 1);
}

#[test]
fn a_file_joins_its_cached_record_without_counting_it_twice() {
    let dir = TestDir::new();
    let cache = ReadCache::new(MAX_OPEN_FILES);
    let revision = cache.revision();
    cache.store_object(revision, cached("name", None));
    let charged = cache.read().charged;
    cache.store_object(revision, cached("name", Some(open_file(&dir))));
    let contents = cache.read();
    assert!(
        contents
            .object("bucket", "name")
            .unwrap()
            .download
            .is_some()
    );
    assert_eq!(contents.charged, charged);
    assert_eq!(contents.object_count, 1);
    assert_eq!(contents.open_files, 1);
}

/// A read that began before a write can finish after it, holding the previous state.
#[test]
fn an_answer_read_before_a_write_is_dropped() {
    let cache = ReadCache::new(MAX_OPEN_FILES);
    let revision = cache.revision();
    cache.invalidate();
    cache.store_object(revision, cached("name", None));
    cache.store_bucket_cors(revision, "bucket", true);
    let contents = cache.read();
    assert!(contents.object("bucket", "name").is_none());
    assert!(contents.bucket_cors.is_empty());
}

#[test]
fn a_write_clears_everything_and_closes_the_files() {
    let dir = TestDir::new();
    let cache = ReadCache::new(MAX_OPEN_FILES);
    let revision = cache.revision();
    let file = open_file(&dir);
    cache.store_object(revision, cached("name", Some(file.clone())));
    cache.store_bucket_cors(revision, "bucket", true);
    assert_eq!(Arc::strong_count(&file), 2);
    cache.invalidate();
    assert_eq!(Arc::strong_count(&file), 1);
    let contents = cache.read();
    assert_eq!(contents.object_count, 0);
    assert_eq!(contents.open_files, 0);
    assert_eq!(contents.charged, 0);
    assert!(contents.bucket_cors.is_empty());
}

#[test]
fn nonexistent_buckets_cannot_grow_the_answers_without_bound() {
    let cache = ReadCache::new(MAX_OPEN_FILES);
    let revision = cache.revision();
    for index in 0..=MAX_BUCKETS {
        cache.store_bucket_cors(revision, &index.to_string(), false);
    }
    assert_eq!(cache.read().bucket_cors.len(), MAX_BUCKETS);
}

#[tokio::test]
async fn hits_stop_when_the_catalog_closes() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    assert!(!catalog.bucket_has_cors("bucket").await.unwrap());
    catalog.close().await.unwrap();
    assert!(metadata(&catalog, "object").await.is_err());
    assert!(download(&catalog, "object").await.is_err());
    assert!(catalog.bucket_has_cors("bucket").await.is_err());
}

/// The directory belongs to the next owner after a close, even while this handle lives on.
#[tokio::test]
async fn closing_releases_every_kept_file() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    assert_eq!(catalog.0.read_cache.read().open_files, 1);
    catalog.close().await.unwrap();
    assert_eq!(catalog.0.read_cache.read().open_files, 0);
}

/// Nothing changed a row here, so only the poison check can stop the hit.
#[tokio::test]
async fn hits_stop_while_a_rollback_stays_unconfirmed() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    assert!(!catalog.bucket_has_cors("bucket").await.unwrap());
    catalog
        .0
        .fail_rollback
        .store(true, std::sync::atomic::Ordering::Release);
    let failed = catalog
        .run::<(), crate::bucket::CatalogError>(|connection| {
            sql::exec(connection, "BEGIN IMMEDIATE", []).unwrap();
            panic!("injected catalog panic")
        })
        .await;
    assert!(failed.is_err());
    assert!(metadata(&catalog, "object").await.is_err());
    assert!(download(&catalog, "object").await.is_err());
    assert!(catalog.bucket_has_cors("bucket").await.is_err());
    catalog
        .0
        .fail_rollback
        .store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(metadata(&catalog, "object").await.unwrap().size, 8);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn hits_line_up_behind_an_exclusive_job() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    let exclusive = catalog
        .wait_exclusive(TimeLimit::from_millis(60_000).deadline())
        .await
        .unwrap();
    let waiting = {
        let catalog = catalog.clone();
        tokio::spawn(async move { metadata(&catalog, "object").await })
    };
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(!waiting.is_finished());
    drop(exclusive);
    waiting.await.unwrap().unwrap();
    catalog.close().await.unwrap();
}

/// A hit waits for nothing, so without the budget charge a task reading in a loop never gives its
/// worker back. This runtime has one worker, so the second task runs only if the first yields.
#[tokio::test]
async fn reading_hits_in_a_loop_lets_other_tasks_run() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    put(&catalog, "object", &b"contents"[..]).await;
    download(&catalog, "object").await.unwrap();
    assert!(!catalog.bucket_has_cors("bucket").await.unwrap());
    let other_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let other = {
        let other_ran = other_ran.clone();
        tokio::spawn(async move { other_ran.store(true, Ordering::Release) })
    };
    let reading = async {
        while !other_ran.load(Ordering::Acquire) {
            metadata(&catalog, "object").await.unwrap();
            download(&catalog, "object").await.unwrap();
            catalog.bucket_has_cors("bucket").await.unwrap();
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), reading)
        .await
        .unwrap();
    other.await.unwrap();
    catalog.close().await.unwrap();
}

/// The property everything above serves: once a write returned, no read that starts afterwards
/// may see anything older, under real concurrency and with files moving in and out of the cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_read_sees_older_than_the_last_finished_write() {
    let dir = TestDir::new();
    let catalog = open_catalog(&dir).await;
    let first = put(&catalog, "object", &b"first"[..]).await;
    let finished = Arc::new(AtomicI64::new(first.generation));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader_tasks: Vec<_> = (0..4)
        .map(|index| {
            let (catalog, finished, done) = (catalog.clone(), finished.clone(), done.clone());
            tokio::spawn(async move {
                while !done.load(Ordering::Acquire) {
                    let at_least = finished.load(Ordering::Acquire);
                    let generation = if index % 2 == 0 {
                        metadata(&catalog, "object").await.unwrap().generation
                    } else {
                        let (record, bytes) = download(&catalog, "object").await.unwrap();
                        assert_eq!(crc32c::crc32c(&bytes), record.crc32c);
                        record.generation
                    };
                    assert!(generation >= at_least, "{generation} < {at_least}");
                }
            })
        })
        .collect();
    for round in 0_u32..300 {
        // Each commit's fsync leaves the reads time to warm the cache, so they exercise hits as
        // well as misses.
        let written = put(&catalog, "object", round.to_be_bytes().to_vec()).await;
        finished.store(written.generation, Ordering::Release);
    }
    done.store(true, Ordering::Release);
    for task in reader_tasks {
        task.await.unwrap();
    }
    catalog.close().await.unwrap();
}

/// The kernel's guarantee is what lets a worker call this, so it must really return the bytes.
#[cfg(target_os = "linux")]
#[test]
fn bytes_in_the_page_cache_read_without_a_blocking_call() {
    use std::io::Read;

    let dir = TestDir::new();
    let path = dir.0.join("blob");
    std::fs::write(&path, b"contents").unwrap();
    // A plain read pulls the pages in, whatever the file system did with the write.
    let mut file = File::open(&path).unwrap();
    file.read_to_end(&mut Vec::new()).unwrap();
    let mut bytes = [0; 4];
    // A file system without RWF_NOWAIT support answers None, which callers handle by blocking.
    if let Some(read) = crate::fsutil::read_cached_at(&file, &mut bytes, 4) {
        assert_eq!(bytes[..read], b"ents"[..read]);
    }
    assert_eq!(crate::fsutil::read_cached_at(&file, &mut bytes, 8), None);
}
