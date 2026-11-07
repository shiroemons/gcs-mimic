use super::*;
use crate::{
    auth::Binding,
    object::ReadAhead,
    storage::test_fixtures::{PERMISSIVE, body, bucket, live, permissive_upload},
    test_support::{TestDir, eventually_async},
    time_limit::TimeLimit,
};
use bytes::Bytes;
use std::{
    sync::{atomic::AtomicUsize, mpsc},
    time::Duration,
};
use tokio::time::timeout;

/// Long enough that no test reaches it unless the behaviour under test is broken.
const LONG: Duration = Duration::from_secs(60);
const LONG_LIMIT: TimeLimit = TimeLimit::from_millis(60_000);

async fn open_with_object(dir: &TestDir) -> Catalog {
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    put(&catalog, "object", b"contents").await;
    catalog
}

async fn put(
    catalog: &Catalog,
    name: &str,
    contents: &'static [u8],
) -> crate::object::ObjectRecord {
    catalog
        .put_object(
            permissive_upload("bucket", name, 1 << 20),
            body(vec![Bytes::from_static(contents)]),
        )
        .await
        .unwrap()
}

fn pool(catalog: &Catalog) -> &ReadPool {
    catalog.0.read_pool.as_ref().unwrap()
}

/// Waits until a read that finished on its thread has also given its slot back.
async fn all_slots_free(catalog: &Catalog) {
    let pool = pool(catalog);
    let capacity = pool.capacity as usize;
    eventually_async("every read slot to be free", || async {
        pool.slots.available_permits() == capacity
    })
    .await;
}

#[tokio::test]
async fn reads_answer_while_the_writer_is_busy() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let served = catalog.pooled_reads();
    let writer = catalog.0.execution.clone().acquire_owned().await.unwrap();
    let record = timeout(
        LONG,
        catalog.get_object(live("bucket", "object"), Default::default(), PERMISSIVE),
    )
    .await
    .unwrap()
    .unwrap();
    drop(writer);
    assert_eq!(record.size, 8);
    assert_eq!(catalog.pooled_reads(), served + 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_read_sees_every_write_that_finished_before_it() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    for _ in 0..32 {
        let written = put(&catalog, "object", b"next").await;
        let read = catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(read.generation, written.generation);
    }
    catalog.close().await.unwrap();
}

/// A reader's snapshot can select a blob that reclamation unlinks before the open, and a
/// retried read must then answer from the serialized path instead of failing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overwrites_neither_fail_nor_mix_a_concurrent_read() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let writes = 200;
    let reader_loops: Vec<_> = (0..4)
        .map(|_| {
            let catalog = catalog.clone();
            tokio::spawn(async move {
                let mut reads = 0_usize;
                while !catalog.0.admission.is_closed() {
                    let (record, mut reader) = catalog
                        .open_object_keyed(
                            live("bucket", "object"),
                            Default::default(),
                            PERMISSIVE,
                            None,
                            ReadAhead::Body,
                        )
                        .await
                        .map_err(|error| error.to_string())?;
                    let mut bytes = Vec::new();
                    while let Some(chunk) = reader.next_chunk().await.unwrap() {
                        bytes.extend_from_slice(&chunk);
                    }
                    assert_eq!(crc32c::crc32c(&bytes), record.crc32c);
                    reads += 1;
                    if reads >= 2000 {
                        break;
                    }
                }
                Ok::<_, String>(reads)
            })
        })
        .collect();
    for round in 0..writes {
        let contents: &'static [u8] = if round % 2 == 0 {
            b"even"
        } else {
            b"odd bytes"
        };
        put(&catalog, "object", contents).await;
    }
    for reader_loop in reader_loops {
        reader_loop.await.unwrap().unwrap();
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_transient_failure_on_a_reader_is_retried_on_the_serialized_path() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let path = catalog
        .run_read_on(move |_, path| {
            counted.fetch_add(1, Ordering::Relaxed);
            match path {
                ReadPath::Pooled => Err(crate::storage::failure::object_unavailable(
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                )),
                ReadPath::Serialized => Ok::<_, ObjectError>(path),
            }
        })
        .await
        .unwrap();
    assert_eq!(path, ReadPath::Serialized);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_definite_answer_from_a_reader_is_not_retried() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let result: Result<(), _> = catalog
        .run_read(move |_| {
            counted.fetch_add(1, Ordering::Relaxed);
            Err(ObjectError::ObjectNotFound)
        })
        .await;
    assert!(matches!(result, Err(ObjectError::ObjectNotFound)));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    catalog.close().await.unwrap();
}

/// The connection is what enforces the rule that a pooled read changes nothing: a write fails
/// there, and the serialized path answers as it always did.
#[tokio::test]
async fn a_write_cannot_happen_on_a_reader() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let paths = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    let changed = catalog
        .run_read_on(move |connection, path| {
            seen.lock().unwrap().push(path);
            Ok::<_, CatalogError>(sql::exec(
                connection,
                "UPDATE buckets SET labels = labels WHERE name = 'bucket'",
                [],
            )?)
        })
        .await
        .unwrap();
    assert_eq!(changed, 1);
    assert_eq!(
        *paths.lock().unwrap(),
        [ReadPath::Pooled, ReadPath::Serialized]
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn reads_line_up_behind_an_exclusive_job() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let exclusive = catalog.wait_exclusive(LONG_LIMIT.deadline()).await.unwrap();
    let served = catalog.pooled_reads();
    let waiting = {
        let catalog = catalog.clone();
        tokio::spawn(async move {
            catalog
                .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
                .await
        })
    };
    for _ in 0..64 {
        tokio::task::yield_now().await;
    }
    assert!(!waiting.is_finished());
    drop(exclusive);
    waiting.await.unwrap().unwrap();
    assert_eq!(catalog.pooled_reads(), served);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_exclusive_job_waits_for_a_running_read() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let (started, running) = tokio::sync::oneshot::channel();
    let started = Mutex::new(Some(started));
    let read = {
        let catalog = catalog.clone();
        tokio::spawn(async move {
            catalog
                .run_read(move |_| {
                    if let Some(started) = started.lock().unwrap().take() {
                        started.send(()).unwrap();
                    }
                    released.lock().unwrap().recv().unwrap();
                    Ok::<_, CatalogError>(())
                })
                .await
        })
    };
    running.await.unwrap();
    let busy = catalog
        .wait_exclusive(TimeLimit::from_millis(50).deadline())
        .await;
    assert!(matches!(
        busy,
        Err(super::super::admin::AdminError::Busy { .. })
    ));
    release.send(()).unwrap();
    read.await.unwrap().unwrap();
    drop(catalog.wait_exclusive(LONG_LIMIT.deadline()).await.unwrap());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn reads_that_do_not_overlap_keep_to_one_reader() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    for _ in 0..8 {
        catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
    }
    assert_eq!(pool(&catalog).reader_count(), 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_caller_that_goes_away_returns_its_reader() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let (started, running) = tokio::sync::oneshot::channel();
    let started = Mutex::new(Some(started));
    let read = {
        let catalog = catalog.clone();
        tokio::spawn(async move {
            catalog
                .run_read(move |_| {
                    if let Some(started) = started.lock().unwrap().take() {
                        started.send(()).unwrap();
                    }
                    released.lock().unwrap().recv().unwrap();
                    Ok::<_, CatalogError>(())
                })
                .await
        })
    };
    running.await.unwrap();
    read.abort();
    assert!(read.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    all_slots_free(&catalog).await;
    assert_eq!(pool(&catalog).reader_count(), 1);
    catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(pool(&catalog).reader_count(), 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_panic_on_a_reader_costs_only_that_reader() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let answer = catalog
        .run_read_on(|_, path| match path {
            ReadPath::Pooled => panic!("reader fixture panic"),
            ReadPath::Serialized => Ok::<_, CatalogError>(7),
        })
        .await
        .unwrap();
    assert_eq!(answer, 7);
    all_slots_free(&catalog).await;
    assert_eq!(pool(&catalog).reader_count(), 0);
    let served = catalog.pooled_reads();
    catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(catalog.pooled_reads(), served + 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn close_ends_the_readers_before_the_writer_removes_the_wal() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(pool(&catalog).reader_count(), 1);
    catalog.close().await.unwrap();
    assert!(!dir.0.join("catalog.sqlite3-wal").exists());
    let result = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await;
    assert!(matches!(result, Err(ObjectError::Unavailable(cause))
        if cause.failure() == Some(crate::storage::failure::BackendFailure::CatalogClosed)));
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    reopened
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    reopened.close().await.unwrap();
}

/// A RAM catalog's database exists only inside the writer's connection.
#[tokio::test]
async fn a_ram_catalog_reads_on_the_serialized_path() {
    let catalog = Catalog::open_memory_for_server(
        1 << 20,
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
    put(&catalog, "object", b"contents").await;
    let record = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(record.size, 8);
    assert_eq!(catalog.pooled_reads(), 0);
    catalog.close().await.unwrap();
}

async fn publicly_readable(catalog: &Catalog) -> bool {
    catalog
        .open_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap()
        .1
        .publicly_readable
}

async fn set_members(catalog: &Catalog, role: &str, member: &str) {
    let mut policy = catalog.get_policy("bucket", PERMISSIVE).await.unwrap();
    policy.bindings.push(Binding {
        role: role.into(),
        members: vec![member.into()],
    });
    catalog
        .set_policy("bucket", policy, PERMISSIVE)
        .await
        .unwrap();
}

/// A connection that already served reads must pick up a later change of the simulated parent
/// policy, which lives in each connection's own TEMP table.
#[tokio::test]
async fn a_parent_policy_change_reaches_existing_readers() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    assert!(!publicly_readable(&catalog).await);
    set_members(&catalog, "roles/storage.objectViewer", "allUsers").await;
    assert!(publicly_readable(&catalog).await);
    let readers = pool(&catalog).reader_count();
    catalog
        .configure_public_access_prevention(&["project".to_owned()])
        .await
        .unwrap();
    assert!(!publicly_readable(&catalog).await);
    catalog
        .configure_public_access_prevention(&[])
        .await
        .unwrap();
    assert!(publicly_readable(&catalog).await);
    assert_eq!(pool(&catalog).reader_count(), readers);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn only_a_grant_to_all_users_that_allows_reading_makes_an_object_public() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    assert!(!publicly_readable(&catalog).await);
    // No identity matches these, although the first names the public group.
    set_members(
        &catalog,
        "roles/storage.objectViewer",
        "allAuthenticatedUsers",
    )
    .await;
    assert!(!publicly_readable(&catalog).await);
    set_members(&catalog, "roles/storage.objectCreator", "allUsers").await;
    assert!(!publicly_readable(&catalog).await);
    set_members(&catalog, "roles/storage.objectViewer", "allUsers").await;
    assert!(publicly_readable(&catalog).await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_object_acl_for_all_users_makes_an_object_public() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    assert!(!publicly_readable(&catalog).await);
    let target = live("bucket", "object");
    catalog
        .update_object_metadata(
            target,
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                acl: Some(vec![crate::acl::AclEntry {
                    entity: "allUsers".into(),
                    role: crate::acl::AclRole::Reader,
                }]),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert!(publicly_readable(&catalog).await);
    catalog.close().await.unwrap();
}
