use bytes::Bytes;
use gcs_mimic::{
    bucket::{BucketRecord, CatalogError},
    clock::Clock,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, UploadChunk},
    storage::{Catalog, ObjectReader},
};
use md5::{Digest, Md5};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

#[derive(Default)]
struct FixedClock(AtomicU64);

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_micros(self.0.load(Ordering::Relaxed))
    }
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "gcs-mimic-objects-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.0.exists() {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

async fn setup(dir: &TestDir) -> Catalog {
    let clock = Arc::new(FixedClock(AtomicU64::new(1_234_567)));
    let catalog = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    catalog
        .insert_bucket(BucketRecord {
            name: "bucket".into(),
            project: "project".into(),
            location: "US".into(),
            location_type: "multi-region".into(),
            storage_class: "NEARLINE".into(),
            labels: BTreeMap::new(),
            versioning_enabled: false,
            default_event_based_hold: false,
            metageneration: 1,
            time_created: UNIX_EPOCH,
            updated: UNIX_EPOCH,
        })
        .await
        .unwrap();
    catalog
}

fn body(parts: Vec<Bytes>) -> mpsc::Receiver<Result<UploadChunk, io::Error>> {
    let (sender, receiver) = mpsc::channel(2);
    tokio::spawn(async move {
        for part in parts {
            if sender.send(Ok(UploadChunk::Data(part))).await.is_err() {
                return;
            }
        }
        let _ = sender.send(Ok(UploadChunk::End)).await;
    });
    receiver
}

async fn put(
    catalog: &Catalog,
    name: &str,
    data: &[u8],
    conditions: ObjectConditions,
) -> Result<ObjectRecord, ObjectError> {
    catalog
        .put_object(
            "bucket",
            name,
            "application/test".into(),
            ExpectedHashes::default(),
            conditions,
            u64::MAX,
            body(vec![Bytes::copy_from_slice(data)]),
        )
        .await
}

async fn read(mut reader: ObjectReader) -> Vec<u8> {
    let mut bytes = Vec::new();
    while let Some(chunk) = reader.next_chunk().await.unwrap() {
        assert!(chunk.len() <= 64 * 1024);
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

fn blobs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir.join("blobs"))
        .unwrap()
        .flat_map(|shard| {
            fs::read_dir(shard.unwrap().path())
                .unwrap()
                .map(|file| file.unwrap().path())
        })
        .collect()
}

fn assert_no_uploads(dir: &Path) {
    assert!(blobs(dir).is_empty());
    assert_eq!(fs::read_dir(dir.join("staging")).unwrap().count(), 0);
}

#[tokio::test]
async fn round_trip_exact_hashes_opaque_names_and_restart() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let name = "../../outside/雪/%2F/";
    let expected_md5 = [
        0x25, 0xf9, 0xe7, 0x94, 0x32, 0x3b, 0x45, 0x38, 0x85, 0xf5, 0x18, 0x1f, 0x1b, 0x62, 0x4d,
        0x0b,
    ];
    let record = catalog
        .put_object(
            "bucket",
            name,
            "application/test".into(),
            ExpectedHashes {
                md5: Some(expected_md5),
                crc32c: Some(0xe306_9283),
            },
            ObjectConditions::default(),
            9,
            body(vec![
                Bytes::from_static(b"123"),
                Bytes::from_static(b"456789"),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(record.md5, expected_md5);
    assert_eq!(record.crc32c, 0xe306_9283);
    assert_eq!(record.size, 9);
    assert_eq!(record.generation, 1_234_567);
    assert_eq!(record.metageneration, 1);
    assert_eq!(record.storage_class, "NEARLINE");
    assert_eq!(record.content_type, "application/test");
    assert_eq!(
        record.time_created,
        UNIX_EPOCH + Duration::from_millis(1234)
    );
    assert_eq!(record.updated, record.time_created);
    assert_eq!(record.name, name);
    let paths = blobs(&dir.0);
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].file_name().unwrap().len(), 32);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .get_object("bucket", name, None, ObjectConditions::default())
            .await
            .unwrap(),
        record
    );
    let (selected, reader) = catalog
        .open_object(
            "bucket",
            name,
            Some(record.generation),
            ObjectConditions::default(),
        )
        .await
        .unwrap();
    assert_eq!(selected, record);
    assert_eq!(read(reader).await, b"123456789");
    catalog
        .delete_object(
            "bucket",
            name,
            Some(record.generation),
            ObjectConditions::default(),
        )
        .await
        .unwrap();
    assert_no_uploads(&dir.0);
    assert_eq!(
        catalog
            .get_object("bucket", name, None, ObjectConditions::default())
            .await,
        Err(ObjectError::ObjectNotFound)
    );
}

#[tokio::test]
async fn producer_closure_without_eof_never_publishes_partial_bytes() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(Bytes::from_static(b"partial"))))
        .await
        .unwrap();
    drop(sender);
    assert_eq!(
        catalog
            .put_object(
                "bucket",
                "partial",
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                100,
                receiver
            )
            .await,
        Err(ObjectError::BodyAborted("missing body EOF".into()))
    );
    assert_eq!(
        catalog
            .get_object("bucket", "partial", None, ObjectConditions::default())
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    assert_no_uploads(&dir.0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn upload_precheck_rejects_without_waiting_for_body_or_creating_staging() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let original = put(&catalog, "exists", b"old", ObjectConditions::default())
        .await
        .unwrap();
    for (bucket, conditions, expected) in [
        (
            "missing",
            ObjectConditions::default(),
            ObjectError::BucketNotFound,
        ),
        (
            "bucket",
            ObjectConditions {
                if_generation_match: Some(0),
                ..Default::default()
            },
            ObjectError::PreconditionFailed,
        ),
    ] {
        let (sender, receiver) = mpsc::channel(1);
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                catalog.put_object(
                    bucket,
                    "exists",
                    "text/plain".into(),
                    ExpectedHashes::default(),
                    conditions,
                    100,
                    receiver
                )
            )
            .await
            .unwrap(),
            Err(expected)
        );
        assert!(sender.is_closed());
        assert_eq!(blobs(&dir.0).len(), 1);
        assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    }
    assert_eq!(
        catalog
            .get_object("bucket", "exists", None, ObjectConditions::default())
            .await
            .unwrap(),
        original
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn empty_and_multi_chunk_bodies_have_exact_bytes_and_incremental_hashes() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    for data in [
        vec![],
        (0..(3 * 64 * 1024 + 17)).map(|i| (i % 251) as u8).collect(),
    ] {
        let md5: [u8; 16] = Md5::digest(&data).into();
        let crc32c = crc32c::crc32c(&data);
        let record = catalog
            .put_object(
                "bucket",
                "body",
                "application/test".into(),
                ExpectedHashes {
                    md5: Some(md5),
                    crc32c: Some(crc32c),
                },
                ObjectConditions::default(),
                data.len() as u64,
                body(data.chunks(31_337).map(Bytes::copy_from_slice).collect()),
            )
            .await
            .unwrap();
        assert_eq!(
            (record.size, record.md5, record.crc32c),
            (data.len() as u64, md5, crc32c)
        );
        let (_, reader) = catalog
            .open_object("bucket", "body", None, ObjectConditions::default())
            .await
            .unwrap();
        assert_eq!(read(reader).await, data);
    }
    // A single large incoming frame must also be split into bounded filesystem writes.
    let data = vec![42; 4 * 64 * 1024];
    put(&catalog, "large-frame", &data, ObjectConditions::default())
        .await
        .unwrap();
    let (_, reader) = catalog
        .open_object("bucket", "large-frame", None, ObjectConditions::default())
        .await
        .unwrap();
    assert_eq!(read(reader).await, data);
}

#[tokio::test]
async fn limits_bad_hashes_and_body_errors_leave_no_generation_or_blob() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    for (max, expected, error) in [
        (2, ExpectedHashes::default(), ObjectError::TooLarge),
        (
            10,
            ExpectedHashes {
                md5: Some([0; 16]),
                crc32c: None,
            },
            ObjectError::ChecksumMismatch,
        ),
        (
            10,
            ExpectedHashes {
                md5: None,
                crc32c: Some(1),
            },
            ObjectError::ChecksumMismatch,
        ),
    ] {
        assert_eq!(
            catalog
                .put_object(
                    "bucket",
                    "bad",
                    "text/plain".into(),
                    expected,
                    ObjectConditions::default(),
                    max,
                    body(vec![Bytes::from_static(b"abc")])
                )
                .await,
            Err(error)
        );
        assert_eq!(
            catalog
                .get_object("bucket", "bad", None, ObjectConditions::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        assert_no_uploads(&dir.0);
    }
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(Bytes::from_static(b"partial"))))
        .await
        .unwrap();
    sender
        .send(Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "cut short",
        )))
        .await
        .unwrap();
    drop(sender);
    assert_eq!(
        catalog
            .put_object(
                "bucket",
                "aborted",
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                100,
                receiver
            )
            .await,
        Err(ObjectError::BodyAborted("cut short".into()))
    );
    assert_no_uploads(&dir.0);
    let original = put(
        &catalog,
        "preserved",
        b"original",
        ObjectConditions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        catalog
            .put_object(
                "bucket",
                "preserved",
                "text/plain".into(),
                ExpectedHashes {
                    md5: Some([0; 16]),
                    crc32c: None
                },
                ObjectConditions::default(),
                100,
                body(vec![Bytes::from_static(b"replacement")])
            )
            .await,
        Err(ObjectError::ChecksumMismatch)
    );
    assert_eq!(
        catalog
            .get_object("bucket", "preserved", None, ObjectConditions::default())
            .await
            .unwrap(),
        original
    );
    assert_eq!(blobs(&dir.0).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_create_if_absent_has_exactly_one_winner_and_no_orphans() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let catalog = catalog.clone();
        tasks.spawn(async move {
            put(
                &catalog,
                "same",
                b"data",
                ObjectConditions {
                    if_generation_match: Some(0),
                    ..Default::default()
                },
            )
            .await
        });
    }
    let mut winners = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(_) => winners += 1,
            Err(error) => assert_eq!(error, ObjectError::PreconditionFailed),
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(blobs(&dir.0).len(), 1);
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

fn conditions(generation: i64, index: usize, pass: bool) -> ObjectConditions {
    match index {
        0 => ObjectConditions {
            if_generation_match: Some(generation + i64::from(!pass)),
            ..Default::default()
        },
        1 => ObjectConditions {
            if_generation_not_match: Some(generation + i64::from(pass)),
            ..Default::default()
        },
        2 => ObjectConditions {
            if_metageneration_match: Some(if pass { 1 } else { 2 }),
            ..Default::default()
        },
        _ => ObjectConditions {
            if_metageneration_not_match: Some(if pass { 2 } else { 1 }),
            ..Default::default()
        },
    }
}

#[tokio::test]
async fn all_four_preconditions_apply_to_reads_uploads_and_deletes() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let mut record = put(
        &catalog,
        "conditions",
        b"first",
        ObjectConditions::default(),
    )
    .await
    .unwrap();
    for index in 0..4 {
        let fail = conditions(record.generation, index, false);
        let read_error = if index % 2 == 1 {
            ObjectError::NotModified
        } else {
            ObjectError::PreconditionFailed
        };
        assert_eq!(
            catalog.get_object("bucket", "conditions", None, fail).await,
            Err(read_error)
        );
        assert_eq!(
            catalog
                .open_object("bucket", "conditions", None, fail)
                .await
                .err(),
            Some(if index % 2 == 1 {
                ObjectError::NotModified
            } else {
                ObjectError::PreconditionFailed
            })
        );
        assert_eq!(
            put(&catalog, "conditions", b"bad", fail).await,
            Err(ObjectError::PreconditionFailed)
        );
        assert_eq!(
            catalog
                .delete_object("bucket", "conditions", None, fail)
                .await,
            Err(ObjectError::PreconditionFailed)
        );
        assert_eq!(blobs(&dir.0).len(), 1);
        let pass = conditions(record.generation, index, true);
        assert_eq!(
            catalog
                .get_object("bucket", "conditions", None, pass)
                .await
                .unwrap(),
            record
        );
        assert!(
            catalog
                .open_object("bucket", "conditions", None, pass)
                .await
                .is_ok()
        );
        let next = put(&catalog, "conditions", b"replacement", pass)
            .await
            .unwrap();
        assert!(next.generation > record.generation);
        record = next;
    }
    for index in 0..4 {
        catalog
            .delete_object(
                "bucket",
                "conditions",
                None,
                conditions(record.generation, index, true),
            )
            .await
            .unwrap();
        record = put(&catalog, "conditions", b"new", ObjectConditions::default())
            .await
            .unwrap();
    }
    let both = ObjectConditions {
        if_generation_match: Some(record.generation),
        if_generation_not_match: Some(record.generation),
        ..Default::default()
    };
    assert_eq!(
        put(&catalog, "conditions", b"bad", both).await,
        Err(ObjectError::PreconditionFailed)
    );
    for absent in [
        ObjectConditions {
            if_generation_match: Some(1),
            ..Default::default()
        },
        ObjectConditions {
            if_generation_not_match: Some(0),
            ..Default::default()
        },
        ObjectConditions {
            if_generation_not_match: Some(1),
            ..Default::default()
        },
        ObjectConditions {
            if_metageneration_match: Some(1),
            ..Default::default()
        },
        ObjectConditions {
            if_metageneration_not_match: Some(2),
            ..Default::default()
        },
    ] {
        assert_eq!(
            put(&catalog, "absent", b"bad", absent).await,
            Err(ObjectError::PreconditionFailed)
        );
    }
    put(
        &catalog,
        "absent",
        b"good",
        ObjectConditions {
            if_generation_match: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    put(
        &catalog,
        "absent",
        b"good",
        ObjectConditions {
            if_generation_not_match: Some(0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn generation_selection_nonempty_bucket_and_open_readers_survive_unlink() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let first = put(
        &catalog,
        "object",
        b"old bytes",
        ObjectConditions::default(),
    )
    .await
    .unwrap();
    let old_path = blobs(&dir.0).pop().unwrap();
    let (_, old_reader) = catalog
        .open_object(
            "bucket",
            "object",
            Some(first.generation),
            ObjectConditions::default(),
        )
        .await
        .unwrap();
    let second = put(
        &catalog,
        "object",
        b"new bytes",
        ObjectConditions::default(),
    )
    .await
    .unwrap();
    assert_eq!(second.generation, first.generation + 1);
    assert!(!old_path.exists());
    assert_eq!(read(old_reader).await, b"old bytes");
    assert_eq!(
        catalog
            .get_object(
                "bucket",
                "object",
                Some(first.generation),
                ObjectConditions::default()
            )
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    assert_eq!(
        catalog
            .open_object(
                "bucket",
                "object",
                Some(first.generation),
                ObjectConditions::default()
            )
            .await
            .err(),
        Some(ObjectError::ObjectNotFound)
    );
    assert_eq!(
        catalog
            .delete_object(
                "bucket",
                "object",
                Some(first.generation),
                ObjectConditions::default()
            )
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    assert_eq!(
        catalog.delete_bucket("bucket", None, None).await,
        Err(CatalogError::NotEmpty)
    );
    let (_, reader) = catalog
        .open_object(
            "bucket",
            "object",
            Some(second.generation),
            ObjectConditions::default(),
        )
        .await
        .unwrap();
    catalog
        .delete_object(
            "bucket",
            "object",
            Some(second.generation),
            ObjectConditions::default(),
        )
        .await
        .unwrap();
    assert_eq!(read(reader).await, b"new bytes");
    assert_no_uploads(&dir.0);
    assert_eq!(
        catalog
            .delete_object("bucket", "object", None, ObjectConditions::default())
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    catalog.delete_bucket("bucket", None, None).await.unwrap();
    assert_eq!(
        catalog
            .get_object("bucket", "object", None, ObjectConditions::default())
            .await,
        Err(ObjectError::BucketNotFound)
    );
    assert_eq!(
        put(&catalog, "object", b"bad", ObjectConditions::default()).await,
        Err(ObjectError::BucketNotFound)
    );
    assert_no_uploads(&dir.0);
}

#[tokio::test]
async fn deleted_generations_are_never_reused_after_restart_or_backward_clock() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let first = put(&catalog, "identity", b"first", ObjectConditions::default())
        .await
        .unwrap();
    catalog
        .delete_object("bucket", "identity", None, ObjectConditions::default())
        .await
        .unwrap();
    let second = put(&catalog, "identity", b"second", ObjectConditions::default())
        .await
        .unwrap();
    assert_ne!(first.generation, second.generation);
    catalog
        .delete_object("bucket", "identity", None, ObjectConditions::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&dir.0, Arc::new(FixedClock(AtomicU64::new(100))))
        .await
        .unwrap();
    let third = put(&catalog, "identity", b"third", ObjectConditions::default())
        .await
        .unwrap();
    assert_ne!(third.generation, first.generation);
    assert_ne!(third.generation, second.generation);
    for stale in [first.generation, second.generation] {
        assert_eq!(
            catalog
                .get_object(
                    "bucket",
                    "identity",
                    Some(stale),
                    ObjectConditions::default()
                )
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        assert_eq!(
            catalog
                .delete_object(
                    "bucket",
                    "identity",
                    None,
                    ObjectConditions {
                        if_generation_match: Some(stale),
                        ..Default::default()
                    }
                )
                .await,
            Err(ObjectError::PreconditionFailed)
        );
    }
    assert_eq!(
        catalog
            .get_object("bucket", "identity", None, ObjectConditions::default())
            .await
            .unwrap(),
        third
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn unreleased_v2_catalog_is_rejected_without_changing_data() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    put(&catalog, "identity", b"first", ObjectConditions::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .execute_batch("DROP TABLE object_generation; PRAGMA user_version = 2;")
        .unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let before = fs::read(&path).unwrap();
    fs::write(dir.0.join("staging/sentinel"), b"untouched").unwrap();
    let error = Catalog::open(&dir.0).await.err().expect("reject v2");
    assert!(
        error
            .to_string()
            .contains("unreleased catalog schema version 2")
    );
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(dir.0.join("staging/sentinel").exists());
    assert_eq!(blobs(&dir.0).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conditional_overwrite_delete_and_bucket_delete_upload_races_are_atomic() {
    let mut overwrite_wins = 0;
    let mut object_delete_wins = 0;
    let mut upload_wins = 0;
    let mut bucket_delete_wins = 0;
    for iteration in 0..12 {
        let dir = TestDir::new();
        let catalog = setup(&dir).await;
        let first = put(&catalog, "race", b"first", ObjectConditions::default())
            .await
            .unwrap();
        let conditions = ObjectConditions {
            if_generation_match: Some(first.generation),
            ..Default::default()
        };
        let deleting = catalog.clone();
        let (overwrite, delete) = race_upload(
            &catalog,
            &dir.0,
            "race",
            conditions,
            async move {
                deleting
                    .delete_object("bucket", "race", None, conditions)
                    .await
            },
            iteration,
        )
        .await;
        match (overwrite, delete) {
            (Ok(next), Err(ObjectError::PreconditionFailed)) => {
                overwrite_wins += 1;
                assert_eq!(
                    catalog
                        .get_object("bucket", "race", None, ObjectConditions::default())
                        .await
                        .unwrap(),
                    next
                );
                catalog
                    .delete_object("bucket", "race", None, ObjectConditions::default())
                    .await
                    .unwrap();
            }
            (Err(ObjectError::PreconditionFailed), Ok(())) => {
                object_delete_wins += 1;
                assert_eq!(
                    catalog
                        .get_object("bucket", "race", None, ObjectConditions::default())
                        .await,
                    Err(ObjectError::ObjectNotFound)
                );
            }
            result => panic!("conditional mutations must have exactly one winner: {result:?}"),
        }
        assert_no_uploads(&dir.0);
        let deleting = catalog.clone();
        let (upload, delete) = race_upload(
            &catalog,
            &dir.0,
            "new",
            ObjectConditions::default(),
            async move { deleting.delete_bucket("bucket", None, None).await },
            iteration,
        )
        .await;
        match (upload, delete) {
            (Ok(record), Err(CatalogError::NotEmpty)) => {
                upload_wins += 1;
                assert_eq!(
                    catalog
                        .get_object("bucket", "new", None, ObjectConditions::default())
                        .await
                        .unwrap(),
                    record
                );
                catalog
                    .delete_object("bucket", "new", None, ObjectConditions::default())
                    .await
                    .unwrap();
            }
            (Err(ObjectError::BucketNotFound), Ok(())) => {
                bucket_delete_wins += 1;
                assert_eq!(
                    catalog
                        .get_object("bucket", "new", None, ObjectConditions::default())
                        .await,
                    Err(ObjectError::BucketNotFound)
                );
            }
            result => {
                panic!("bucket deletion and publication cannot leave a dangling object: {result:?}")
            }
        }
        assert_no_uploads(&dir.0);
        catalog.close().await.unwrap();
    }
    assert!(overwrite_wins > 0 && object_delete_wins > 0);
    assert!(upload_wins > 0 && bucket_delete_wins > 0);
    eprintln!(
        "race outcomes: overwrite={overwrite_wins}, object_delete={object_delete_wins}, upload={upload_wins}, bucket_delete={bucket_delete_wins}"
    );
}

async fn race_upload<T: Send + 'static>(
    catalog: &Catalog,
    dir: &Path,
    name: &'static str,
    conditions: ObjectConditions,
    delete: impl std::future::Future<Output = T> + Send + 'static,
    iteration: usize,
) -> (Result<ObjectRecord, ObjectError>, T) {
    let (sender, receiver) = mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(Bytes::from_static(b"replacement"))))
        .await
        .unwrap();
    let worker = catalog.clone();
    let upload = tokio::spawn(async move {
        worker
            .put_object(
                "bucket",
                name,
                "text/plain".into(),
                ExpectedHashes::default(),
                conditions,
                100,
                receiver,
            )
            .await
    });
    // Rendezvous after the advisory check and body write, before transport-confirmed EOF.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let written = fs::read_dir(dir.join("staging"))
                .unwrap()
                .any(|entry| entry.unwrap().metadata().unwrap().len() == 11);
            if written {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let (deleted, deletion) = tokio::sync::oneshot::channel();
    let start = barrier.clone();
    let finish = tokio::spawn(async move {
        start.wait().await;
        // Force delete-first without tying publication order to filesystem latency.
        if iteration % 3 == 1 {
            deletion.await.unwrap();
        }
        sender.send(Ok(UploadChunk::End)).await.unwrap();
    });
    let start = barrier.clone();
    let observer = catalog.clone();
    let delete = tokio::spawn(async move {
        start.wait().await;
        if iteration % 3 == 2 {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(record) = observer
                        .get_object("bucket", name, None, ObjectConditions::default())
                        .await
                        && Some(record.generation) != conditions.if_generation_match
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        let result = delete.await;
        let _ = deleted.send(());
        result
    });
    barrier.wait().await;
    let (upload, delete, finish) = tokio::join!(upload, delete, finish);
    finish.unwrap();
    (upload.unwrap(), delete.unwrap())
}

#[tokio::test]
async fn incompatible_schema_constraints_indexes_and_v1_conflicts_are_read_only() {
    for fixture in [
        "object-constraints",
        "blob-unique",
        "journal-constraints",
        "bucket-index",
        "v1-conflict",
        "hidden-inventory",
    ] {
        let dir = TestDir::new();
        let catalog = setup(&dir).await;
        catalog.close().await.unwrap();
        let path = dir.0.join("catalog.sqlite3");
        let connection = Connection::open(&path).unwrap();
        match fixture {
            "object-constraints" => connection.execute_batch("ALTER TABLE objects RENAME TO old_objects; CREATE TABLE objects AS SELECT * FROM old_objects; DROP TABLE old_objects;").unwrap(),
            "blob-unique" => {
                let sql: String = connection.query_row("SELECT sql FROM sqlite_schema WHERE name='objects'", [], |row| row.get(0)).unwrap();
                connection.execute("DROP TABLE objects", []).unwrap();
                connection.execute_batch(&sql.replace("blob_id TEXT NOT NULL UNIQUE", "blob_id TEXT NOT NULL")).unwrap();
            }
            "journal-constraints" => connection.execute_batch("DROP TABLE pending_blob_deletions; CREATE TABLE pending_blob_deletions(blob_id TEXT);").unwrap(),
            "bucket-index" => connection.execute("DROP INDEX buckets_project_name", []).map(|_| ()).unwrap(),
            "v1-conflict" => connection.execute_batch("DROP TABLE objects; DROP TABLE object_generation; PRAGMA user_version = 1;").unwrap(),
            "hidden-inventory" => connection.execute("CREATE TABLE sqliteXunexpected(value TEXT)", []).map(|_| ()).unwrap(),
            _ => unreachable!(),
        }
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        drop(connection);
        let before = fs::read(&path).unwrap();
        fs::write(dir.0.join("staging/sentinel"), b"must not be recovered").unwrap();
        assert!(Catalog::open(&dir.0).await.is_err(), "{fixture}");
        assert_eq!(fs::read(&path).unwrap(), before, "{fixture}");
        assert!(dir.0.join("staging/sentinel").exists(), "{fixture}");
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                .unwrap(),
            "delete",
            "{fixture}"
        );
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            version,
            "{fixture}"
        );
    }
}

fn orphan(dir: &Path, id: &str) -> PathBuf {
    let path = dir.join("blobs").join(&id[..2]).join(id);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"orphan").unwrap();
    path
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn committed_mutations_succeed_when_reclaim_is_deferred_to_restart() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let first = put(&catalog, "object", b"old", ObjectConditions::default())
        .await
        .unwrap();
    let old_path = blobs(&dir.0).pop().unwrap();
    let old_shard = old_path.parent().unwrap();
    let probe = old_shard.join("reclaim-probe");
    fs::write(&probe, b"probe").unwrap();
    let permissions = fs::metadata(old_shard).unwrap().permissions();
    fs::set_permissions(old_shard, fs::Permissions::from_mode(0o555)).unwrap();
    let denial = fs::remove_file(&probe);
    fs::set_permissions(old_shard, permissions).unwrap();
    match denial {
        Ok(()) => {
            eprintln!("SKIP: read-only shard permits unlink (for example when running as root)");
            catalog.close().await.unwrap();
            return;
        }
        Err(error) => assert_eq!(error.kind(), io::ErrorKind::PermissionDenied),
    }
    fs::remove_file(probe).unwrap();
    let database = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    let worker = catalog.clone();
    let overwrite =
        tokio::spawn(
            async move { put(&worker, "object", b"new", ObjectConditions::default()).await },
        );
    // Change permissions only after rename, even if the new blob shares the old shard.
    tokio::time::timeout(Duration::from_secs(3), async {
        while blobs(&dir.0).len() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let permissions = fs::metadata(old_shard).unwrap().permissions();
    fs::set_permissions(old_shard, fs::Permissions::from_mode(0o555)).unwrap();
    database.execute_batch("ROLLBACK").unwrap();
    let result = overwrite.await.unwrap();
    fs::set_permissions(old_shard, permissions).unwrap();
    let next = result.expect("committed overwrite must succeed despite failed reclaim");
    assert_ne!(next.generation, first.generation);
    assert!(old_path.exists(), "read-only shard prevented reclaim");
    assert_eq!(
        catalog
            .get_object("bucket", "object", None, ObjectConditions::default())
            .await
            .unwrap(),
        next
    );
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let new_path = blobs(&dir.0)
        .into_iter()
        .find(|path| path != &old_path)
        .unwrap();
    let new_shard = new_path.parent().unwrap();
    let permissions = fs::metadata(new_shard).unwrap().permissions();
    fs::set_permissions(new_shard, fs::Permissions::from_mode(0o555)).unwrap();
    let result = catalog
        .delete_object("bucket", "object", None, ObjectConditions::default())
        .await;
    fs::set_permissions(new_shard, permissions).unwrap();
    result.expect("committed delete must succeed despite failed reclaim");
    assert!(new_path.exists());
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(database);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_no_uploads(&dir.0);
    assert_eq!(
        catalog
            .get_object("bucket", "object", None, ObjectConditions::default())
            .await,
        Err(ObjectError::ObjectNotFound)
    );
    let database = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_startup_journal_reclaim_names_blob_and_blocks_readiness_until_fixed() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    catalog.close().await.unwrap();
    let id = "ab000000000000000000000000000000";
    let path = orphan(&dir.0, id);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let database = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database
        .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
        .unwrap();
    drop(database);
    for _ in 0..2 {
        let error = Catalog::open(&dir.0).await.err().unwrap().to_string();
        assert!(
            error.contains(&format!("deletion journal blob {id}:")),
            "{error}"
        );
        assert_eq!(error.matches("catalog unavailable:").count(), 1, "{error}");
        assert!(!error.contains(&dir.0.display().to_string()), "{error}");
    }
    fs::remove_dir(path).unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let database = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn recovery_ignores_os_metadata_and_names_other_stray_entries() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let live = put(&catalog, "live", b"durable", ObjectConditions::default())
        .await
        .unwrap();
    let shard = blobs(&dir.0).pop().unwrap().parent().unwrap().to_owned();
    catalog.close().await.unwrap();
    for parent in [dir.0.join("staging"), dir.0.join("blobs"), shard] {
        for name in [".DS_Store", "._metadata"] {
            fs::write(parent.join(name), b"OS metadata").unwrap();
        }
    }
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .get_object("bucket", "live", None, ObjectConditions::default())
            .await
            .unwrap(),
        live
    );
    catalog.close().await.unwrap();
    for stray in [
        "blobs/unexpected",
        "blobs/aa/invalid-id",
        "staging/unexpected-directory",
    ] {
        let dir = TestDir::new();
        let catalog = setup(&dir).await;
        catalog.close().await.unwrap();
        let path = dir.0.join(stray);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if stray.starts_with("staging") {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"stray").unwrap();
        }
        let error = Catalog::open(&dir.0)
            .await
            .err()
            .expect("stray entry blocks readiness");
        assert!(
            error.to_string().contains(&path.display().to_string()),
            "{error}"
        );
        assert!(path.exists());
    }
}

#[tokio::test]
async fn recovery_clears_staging_orphans_and_pending_deletions_preserving_live_objects() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let live = put(&catalog, "live", b"durable", ObjectConditions::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    fs::write(dir.0.join("staging/unfinished"), b"partial").unwrap();
    let orphan_path = orphan(&dir.0, "aabbccddeeff00112233445566778899");
    let journal_path = orphan(&dir.0, "bbccddeeff00112233445566778899aa");
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO pending_blob_deletions VALUES (?1)",
            ["bbccddeeff00112233445566778899aa"],
        )
        .unwrap();
    // A crash after unlink but before clearing the journal is replayed idempotently too.
    connection
        .execute(
            "INSERT INTO pending_blob_deletions VALUES (?1)",
            ["ccddeeff00112233445566778899aabb"],
        )
        .unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert!(!orphan_path.exists());
    assert!(!journal_path.exists());
    assert_eq!(blobs(&dir.0).len(), 1);
    assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    assert_eq!(
        catalog
            .get_object("bucket", "live", None, ObjectConditions::default())
            .await
            .unwrap(),
        live
    );
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(connection);
    Catalog::open(&dir.0).await.unwrap();
}

#[tokio::test]
async fn recovery_crosses_bounded_pages_and_never_reclaims_a_live_journal_entry() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    put(&catalog, "live", b"orphan", ObjectConditions::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let mut connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let transaction = connection.transaction().unwrap();
    for index in 0..130 {
        let id = format!("d1{index:030x}");
        orphan(&dir.0, &id);
        transaction
            .execute(
                "INSERT INTO objects (bucket, name, generation, metageneration, content_type,
                size, md5, crc32c, storage_class, time_created, updated, blob_id)
                SELECT bucket, ?1, generation, metageneration, content_type,
                size, md5, crc32c, storage_class, time_created, updated, ?2
                FROM objects WHERE name = 'live'",
                [format!("live-{index}"), id],
            )
            .unwrap();
        let id = format!("d2{index:030x}");
        orphan(&dir.0, &id);
        transaction
            .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
            .unwrap();
        orphan(&dir.0, &format!("d3{index:030x}"));
    }
    transaction.commit().unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(blobs(&dir.0).len(), 131);
    let (_, reader) = catalog
        .open_object("bucket", "live-129", None, ObjectConditions::default())
        .await
        .unwrap();
    assert_eq!(read(reader).await, b"orphan");
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    connection
        .execute(
            "INSERT INTO pending_blob_deletions SELECT blob_id FROM objects WHERE name = 'live'",
            [],
        )
        .unwrap();
    drop(connection);
    let error = Catalog::open(&dir.0).await.err().unwrap();
    assert!(
        error
            .to_string()
            .contains("deletion journal references a live blob")
    );
    assert_eq!(blobs(&dir.0).len(), 131);
}

#[tokio::test]
async fn missing_or_truncated_referenced_blob_blocks_readiness_and_releases_lock() {
    for missing in [true, false] {
        let dir = TestDir::new();
        let catalog = setup(&dir).await;
        put(&catalog, "live", b"durable", ObjectConditions::default())
            .await
            .unwrap();
        catalog.close().await.unwrap();
        let path = blobs(&dir.0).pop().unwrap();
        if missing {
            fs::remove_file(&path).unwrap();
        } else {
            fs::write(&path, b"short").unwrap();
        }
        for _ in 0..2 {
            let error = Catalog::open(&dir.0).await.err().unwrap();
            assert!(error.to_string().contains("referenced blob"), "{error}");
            assert!(!error.to_string().contains("already in use"));
        }
    }
}

#[tokio::test]
async fn v1_migration_is_transactional_and_current_schema_reopens_idempotently() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let bucket = catalog.get_bucket("bucket").await.unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch(
            "DROP TABLE rewrite_sessions; DROP TABLE derived_deletions; DROP TABLE derived_inputs; DROP TABLE derived_transfers; ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; DROP TABLE bucket_policies; DROP TABLE policy_revision; DROP TABLE objects; DROP TABLE pending_blob_deletions; DROP TABLE object_generation; DROP TABLE startup_seed; PRAGMA user_version = 1;",
        )
        .unwrap();
    // Force the second DDL statement to fail and verify the first statement rolls back.
    connection
        .execute(
            "CREATE TABLE pending_blob_deletions (incompatible TEXT)",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(Catalog::open(&dir.0).await.is_err());
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'objects'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    connection
        .execute("DROP TABLE pending_blob_deletions", [])
        .unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(catalog.get_bucket("bucket").await.unwrap(), bucket);
    let settings = catalog.get_bucket("bucket").await.unwrap().unwrap();
    assert!(!settings.versioning_enabled && !settings.default_event_based_hold);
    let object = put(&catalog, "migrated", b"body", ObjectConditions::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    for _ in 0..2 {
        let catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            catalog
                .get_object("bucket", "migrated", None, ObjectConditions::default())
                .await
                .unwrap(),
            object
        );
        catalog.close().await.unwrap();
    }
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        11
    );
}

#[tokio::test]
async fn metadata_update_uses_clock_preserves_blob_and_migrates_previous_catalog() {
    use gcs_mimic::object::ObjectMetadataUpdate;

    let dir = TestDir::new();
    fs::create_dir_all(&dir.0).unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE buckets (
                name TEXT PRIMARY KEY COLLATE BINARY, project TEXT NOT NULL,
                location TEXT NOT NULL, location_type TEXT NOT NULL, storage_class TEXT NOT NULL,
                labels TEXT NOT NULL, metageneration INTEGER NOT NULL,
                time_created INTEGER NOT NULL, updated INTEGER NOT NULL
            ) WITHOUT ROWID;
            CREATE INDEX buckets_project_name ON buckets(project, name);
            CREATE TABLE objects (
                bucket TEXT NOT NULL COLLATE BINARY, name TEXT NOT NULL COLLATE BINARY,
                generation INTEGER NOT NULL CHECK(generation > 0),
                metageneration INTEGER NOT NULL CHECK(metageneration > 0), content_type TEXT NOT NULL,
                size INTEGER NOT NULL CHECK(size >= 0), md5 BLOB NOT NULL CHECK(length(md5) = 16),
                crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
                storage_class TEXT NOT NULL, time_created INTEGER NOT NULL, updated INTEGER NOT NULL,
                blob_id TEXT NOT NULL UNIQUE, PRIMARY KEY(bucket, name)
            ) WITHOUT ROWID;
            CREATE TABLE pending_blob_deletions (blob_id TEXT PRIMARY KEY) WITHOUT ROWID;
            CREATE TABLE object_generation (
                singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
            ) WITHOUT ROWID;
            INSERT INTO buckets VALUES ('bucket','project','US','multi-region','NEARLINE','{}',1,0,0);
            INSERT INTO object_generation VALUES (1,1234567);
            PRAGMA user_version = 3;",
        )
        .unwrap();
    let md5: [u8; 16] = Md5::digest(b"unchanged").into();
    let crc = crc32c::crc32c(b"unchanged");
    let blob_id = "a0000000000000000000000000000000";
    connection.execute("INSERT INTO objects VALUES ('bucket','metadata',1234567,1,'application/test',9,?1,?2,'NEARLINE',1234,1234,?3)",rusqlite::params![&md5[..],crc,blob_id]).unwrap();
    fs::create_dir_all(dir.0.join("blobs/a0")).unwrap();
    fs::write(dir.0.join("blobs/a0").join(blob_id), b"unchanged").unwrap();
    let original_paths = blobs(&dir.0);
    let original = ObjectRecord {
        bucket: "bucket".into(),
        name: "metadata".into(),
        generation: 1234567,
        metageneration: 1,
        content_type: "application/test".into(),
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: Default::default(),
        size: 9,
        md5,
        crc32c: crc,
        component_count: None,
        storage_class: "NEARLINE".into(),
        time_created: UNIX_EPOCH + Duration::from_millis(1234),
        updated: UNIX_EPOCH + Duration::from_millis(1234),
        time_deleted: None,
        event_based_hold: false,
    };
    drop(connection);
    let clock = Arc::new(FixedClock(AtomicU64::new(9_876_000)));
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    let settings = catalog.get_bucket("bucket").await.unwrap().unwrap();
    assert!(!settings.versioning_enabled && !settings.default_event_based_hold);
    assert_eq!(
        catalog
            .get_object("bucket", "metadata", None, ObjectConditions::default())
            .await
            .unwrap(),
        original
    );
    let changed = catalog
        .update_object_metadata(
            "bucket",
            "metadata",
            None,
            ObjectConditions::default(),
            ObjectMetadataUpdate {
                content_type: Some(Some("text/plain".into())),
                content_encoding: Some(Some("identity".into())),
                metadata: Some(Some(BTreeMap::from([("key".into(), Some("value".into()))]))),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert_eq!(changed.updated, UNIX_EPOCH + Duration::from_millis(9876));
    assert_eq!(changed.metageneration, 2);
    assert_eq!(changed.generation, original.generation);
    assert_eq!(changed.time_created, original.time_created);
    assert_eq!(changed.md5, original.md5);
    assert_eq!(blobs(&dir.0), original_paths);
    let (_, reader) = catalog
        .open_object("bucket", "metadata", None, ObjectConditions::default())
        .await
        .unwrap();
    assert_eq!(read(reader).await, b"unchanged");
    clock.0.store(10_000_000, Ordering::Relaxed);
    let cleared = catalog
        .update_object_metadata(
            "bucket",
            "metadata",
            None,
            ObjectConditions::default(),
            ObjectMetadataUpdate {
                replace: true,
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert_eq!(cleared.updated, UNIX_EPOCH + Duration::from_millis(10_000));
    assert!(cleared.content_type.is_empty());
    assert!(cleared.metadata.is_empty());
    assert!(cleared.content_encoding.is_none());
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .get_object("bucket", "metadata", None, ObjectConditions::default())
            .await
            .unwrap(),
        cleared
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn metadata_update_races_with_delete_and_upload_preserve_atomic_identity() {
    use gcs_mimic::object::ObjectMetadataUpdate;
    for overwrite in [false, true] {
        let dir = TestDir::new();
        let catalog = setup(&dir).await;
        let original = put(&catalog, "race", b"old bytes", ObjectConditions::default())
            .await
            .unwrap();
        let conditions = ObjectConditions {
            if_generation_match: Some(original.generation),
            if_metageneration_match: Some(1),
            ..Default::default()
        };
        let barrier = Arc::new(tokio::sync::Barrier::new(3));
        let updater = {
            let catalog = catalog.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                catalog
                    .update_object_metadata(
                        "bucket",
                        "race",
                        None,
                        conditions,
                        ObjectMetadataUpdate {
                            metadata: Some(Some(BTreeMap::from([(
                                "key".into(),
                                Some("value".into()),
                            )]))),
                            ..Default::default()
                        },
                        |_| Ok(()),
                    )
                    .await
            })
        };
        let mutation = {
            let catalog = catalog.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                barrier.wait().await;
                if overwrite {
                    put(&catalog, "race", b"new bytes", conditions)
                        .await
                        .map(|_| ())
                } else {
                    catalog
                        .delete_object("bucket", "race", None, conditions)
                        .await
                }
            })
        };
        barrier.wait().await;
        let updated = updater.await.unwrap();
        let mutated = mutation.await.unwrap();
        assert_ne!(updated.is_ok(), mutated.is_ok());
        if updated.is_ok() {
            assert_eq!(mutated, Err(ObjectError::PreconditionFailed));
            let (record, reader) = catalog
                .open_object("bucket", "race", None, ObjectConditions::default())
                .await
                .unwrap();
            assert_eq!(record.generation, original.generation);
            assert_eq!(record.metageneration, 2);
            assert_eq!(record.metadata["key"], "value");
            assert_eq!(read(reader).await, b"old bytes");
        } else if overwrite {
            assert_eq!(updated, Err(ObjectError::PreconditionFailed));
            let (record, reader) = catalog
                .open_object("bucket", "race", None, ObjectConditions::default())
                .await
                .unwrap();
            assert_ne!(record.generation, original.generation);
            assert_eq!(record.metageneration, 1);
            assert!(record.metadata.is_empty());
            assert_eq!(read(reader).await, b"new bytes");
        } else {
            assert_eq!(updated, Err(ObjectError::ObjectNotFound));
            assert_eq!(
                catalog
                    .get_object("bucket", "race", None, ObjectConditions::default())
                    .await,
                Err(ObjectError::ObjectNotFound)
            );
        }
        assert_eq!(fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
        assert_eq!(
            blobs(&dir.0).len(),
            usize::from(overwrite || mutated.is_err())
        );
        catalog.close().await.unwrap();
    }
}

#[test]
fn stalled_uploads_do_not_occupy_blocking_workers_and_admission_is_bounded() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let dir = TestDir::new();
            let catalog = setup(&dir).await;
            let mut tasks = tokio::task::JoinSet::new();
            let mut senders = Vec::new();
            for index in 0..8 {
                let (sender, receiver) = mpsc::channel(1);
                senders.push(sender);
                let catalog = catalog.clone();
                tasks.spawn(async move {
                    catalog
                        .put_object(
                            "bucket",
                            &format!("stalled-{index}"),
                            "text/plain".into(),
                            ExpectedHashes::default(),
                            ObjectConditions::default(),
                            100,
                            receiver,
                        )
                        .await
                });
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while fs::read_dir(dir.0.join("staging")).unwrap().count() != 8 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), catalog.get_bucket("bucket"))
                .await
                .unwrap()
                .unwrap();
            assert!(
                matches!(put(&catalog, "excess", b"bad", ObjectConditions::default()).await,
            Err(ObjectError::Unavailable(message)) if message.contains("upload limit"))
            );
            for sender in senders {
                sender
                    .send(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "abort")))
                    .await
                    .unwrap();
            }
            while let Some(task) = tasks.join_next().await {
                assert_eq!(task.unwrap(), Err(ObjectError::BodyAborted("abort".into())));
            }
            assert_no_uploads(&dir.0);
        });
}

#[tokio::test]
async fn cancelled_upload_keeps_directory_owned_until_close_finishes_cleanup() {
    let dir = TestDir::new();
    let catalog = setup(&dir).await;
    let (sender, receiver) = mpsc::channel(1);
    let worker = catalog.clone();
    let upload = tokio::spawn(async move {
        worker
            .put_object(
                "bucket",
                "cancelled",
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                100,
                receiver,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while fs::read_dir(dir.0.join("staging")).unwrap().count() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    upload.abort();
    assert!(upload.await.unwrap_err().is_cancelled());
    let closer = catalog.clone();
    let close = tokio::spawn(async move { closer.close().await });
    tokio::task::yield_now().await;
    assert!(Catalog::open(&dir.0).await.is_err());
    tokio::time::timeout(Duration::from_secs(5), close)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_no_uploads(&dir.0);
    assert!(sender.is_closed());
    Catalog::open(&dir.0).await.unwrap();
}
