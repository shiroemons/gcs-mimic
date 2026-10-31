use super::*;
use crate::storage::{ResumableChunk, ResumableError, ResumableOptions};
use serde_json::json;

async fn session(catalog: &Catalog) -> crate::storage::ResumableSession {
    let mut options = ResumableOptions::new(BucketAccess::Permissive);
    options.content_type = "application/test".into();
    catalog
        .create_resumable_session("test-bucket", "value", options, None)
        .await
        .unwrap()
}

async fn chunk(
    catalog: &Catalog,
    id: &str,
    bytes: &'static [u8],
    start: u64,
    total: Option<u64>,
    finalize: bool,
) -> Result<Option<serde_json::Value>, ResumableError> {
    let lease = catalog.lock_resumable_session(id, "test-bucket").await?;
    let options = lease.session.options.clone();
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    drop(sender);
    catalog
        .write_resumable_session(
            lease,
            ResumableChunk {
                start,
                length: bytes.len() as u64,
                total,
                finalize,
                open_end: false,
                options,
                catalog_wait: tokio::sync::watch::channel(false).0,
            },
            body,
            catalog.admit_object_upload()?,
            |record| Ok(json!({"generation": record.generation, "size":record.size})),
        )
        .await
}

fn expected() -> crate::hashing::Hashes {
    let mut hash = crate::hashing::Hasher::default();
    hash.update(b"parttail");
    hash.finish()
}

#[tokio::test]
async fn ram_resumable_overlap_preserves_prefix_hashes_and_atomic_completion() {
    let catalog = setup(200_000, true).await;
    let original = put(&catalog, Default::default(), true).await.unwrap();
    let session = session(&catalog).await;
    assert!(
        chunk(&catalog, &session.id, b"part", 0, None, false)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        catalog
            .resumable_session(&session.id, "test-bucket")
            .await
            .unwrap()
            .offset,
        4
    );
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    let completed = chunk(&catalog, &session.id, b"XXXXtail", 0, Some(8), true)
        .await
        .unwrap()
        .unwrap();
    let stored = catalog
        .resumable_session(&session.id, "test-bucket")
        .await
        .unwrap();
    assert_eq!(stored.state, crate::storage::SessionState::Complete);
    assert_eq!(stored.completion.as_ref(), Some(&completed));
    let object = catalog
        .get_object("test-bucket", "value", None, Default::default())
        .await
        .unwrap();
    assert_eq!(
        (object.md5, object.crc32c),
        (expected().md5, expected().crc32c)
    );
    assert_eq!(object.content_type, "application/test");
    let (_, mut reader) = catalog
        .open_object("test-bucket", "value", None, Default::default())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(next) = reader.next_chunk().await.unwrap() {
        bytes.extend_from_slice(&next);
    }
    assert_eq!(bytes, b"parttail");
    assert_eq!(
        catalog
            .cancel_resumable_session(&session.id, "test-bucket")
            .await,
        Err(ResumableError::Gone)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_resumable_capacity_and_checkpoint_failure_restore_bytes_offset_and_hash() {
    let catalog = setup(4096, false).await;
    let original = put(&catalog, Default::default(), true).await.unwrap();
    let session = session(&catalog).await;
    chunk(&catalog, &session.id, b"part", 0, None, false)
        .await
        .unwrap();
    let mut reservations = Vec::new();
    while let Ok(stage) = catalog.0.memory.as_ref().unwrap().stage() {
        reservations.push(stage);
    }
    assert!(
        chunk(&catalog, &session.id, b"tail", 4, Some(8), true)
            .await
            .is_err()
    );
    assert_eq!(
        catalog
            .resumable_session(&session.id, "test-bucket")
            .await
            .unwrap()
            .offset,
        4
    );
    assert_eq!(
        catalog
            .get_object("test-bucket", "value", None, Default::default())
            .await
            .unwrap(),
        original
    );
    drop(reservations);
    catalog.run_object(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER reject_ram_resumable BEFORE UPDATE OF offset ON resumable_sessions BEGIN SELECT RAISE(ABORT, 'injected checkpoint failure'); END;").unwrap();
        Ok(())
    }).await.unwrap();
    assert!(
        chunk(&catalog, &session.id, b"tail", 4, Some(8), true)
            .await
            .is_err()
    );
    assert_eq!(
        catalog
            .resumable_session(&session.id, "test-bucket")
            .await
            .unwrap()
            .offset,
        4
    );
    catalog
        .run_object(|connection| {
            connection
                .execute_batch("DROP TRIGGER reject_ram_resumable")
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    chunk(&catalog, &session.id, b"tail", 4, Some(8), true)
        .await
        .unwrap()
        .unwrap();
    let result = catalog
        .get_object("test-bucket", "value", None, Default::default())
        .await
        .unwrap();
    assert_eq!(
        (result.md5, result.crc32c),
        (expected().md5, expected().crc32c)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_resumable_cancel_expiry_reset_and_close_release_session_leases() {
    let catalog = setup(4096, false).await;
    let active = session(&catalog).await;
    let lease = catalog
        .lock_resumable_session(&active.id, "test-bucket")
        .await
        .unwrap();
    assert!(
        catalog
            .lock_resumable_session(&active.id, "test-bucket")
            .await
            .is_err()
    );
    assert!(matches!(
        catalog.admin(admin::Origin::Seed).reset().await,
        Err(AdminError::Busy { .. })
    ));
    drop(lease);
    chunk(&catalog, &active.id, b"part", 0, None, false)
        .await
        .unwrap();
    catalog
        .cancel_resumable_session(&active.id, "test-bucket")
        .await
        .unwrap();
    assert_eq!(
        catalog.resumable_session(&active.id, "test-bucket").await,
        Err(ResumableError::Gone)
    );
    let expired = session(&catalog).await;
    let id = expired.id.clone();
    catalog
        .run_object(move |connection| {
            connection
                .prepare_cached(
                    "UPDATE resumable_sessions SET created=0,deadline=604800000 WHERE id=?1",
                )
                .unwrap()
                .execute([id])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(
        catalog.resumable_session(&expired.id, "test-bucket").await,
        Err(ResumableError::NotFound)
    );
    let reset = session(&catalog).await;
    catalog.admin(admin::Origin::Seed).reset().await.unwrap();
    assert_eq!(
        catalog.resumable_session(&reset.id, "test-bucket").await,
        Err(ResumableError::NotFound)
    );
    catalog.close().await.unwrap();
    assert!(
        catalog
            .lock_resumable_session(&reset.id, "test-bucket")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn ram_resumable_cancelled_request_discards_tail_and_keeps_the_previous_hash_checkpoint() {
    let catalog = setup(200_000, false).await;
    let active = session(&catalog).await;
    chunk(&catalog, &active.id, b"part", 0, None, false)
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&active.id, "test-bucket")
        .await
        .unwrap();
    let options = lease.session.options.clone();
    let permit = catalog.admit_object_upload().unwrap();
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"hidden"))))
        .unwrap();
    let worker = catalog.clone();
    let request = tokio::spawn(async move {
        worker
            .write_resumable_session(
                lease,
                ResumableChunk {
                    start: 4,
                    length: 6,
                    total: None,
                    finalize: false,
                    open_end: false,
                    options,
                    catalog_wait: tokio::sync::watch::channel(false).0,
                },
                body,
                permit,
                |_| Ok(json!({})),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while sender.capacity() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    tokio::time::timeout(std::time::Duration::from_secs(2), sender.closed())
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&active.id, "test-bucket")
        .await
        .unwrap();
    assert_eq!(lease.session.offset, 4);
    drop(lease);
    chunk(&catalog, &active.id, b"tail", 4, Some(8), true)
        .await
        .unwrap();
    let result = catalog
        .get_object("test-bucket", "value", None, Default::default())
        .await
        .unwrap();
    assert_eq!(
        (result.md5, result.crc32c),
        (expected().md5, expected().crc32c)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_resumable_session_limit_and_expiry_recycle_all_staging_capacity() {
    let catalog = setup(256 * 1024, false).await;
    for _ in 0..128 {
        session(&catalog).await;
    }
    let mut options = ResumableOptions::new(BucketAccess::Permissive);
    options.content_type = "application/test".into();
    assert!(
        catalog
            .create_resumable_session("test-bucket", "value", options, None)
            .await
            .is_err()
    );
    catalog
        .run_object(|connection| {
            connection
                .prepare_cached("UPDATE resumable_sessions SET created=0,deadline=604800000")
                .unwrap()
                .execute([])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.maintain_resumable_sessions().await.unwrap();
    let next = session(&catalog).await;
    chunk(&catalog, &next.id, b"parttail", 0, Some(8), true)
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ram_resumable_close_wakes_a_stalled_body_and_waits_for_owned_cleanup() {
    let catalog = setup(200_000, false).await;
    let active = session(&catalog).await;
    let lease = catalog
        .lock_resumable_session(&active.id, "test-bucket")
        .await
        .unwrap();
    let options = lease.session.options.clone();
    let permit = catalog.admit_object_upload().unwrap();
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"pending"))))
        .unwrap();
    let worker = catalog.clone();
    let request = tokio::spawn(async move {
        worker
            .write_resumable_session(
                lease,
                ResumableChunk {
                    start: 0,
                    length: 7,
                    total: None,
                    finalize: false,
                    open_end: false,
                    options,
                    catalog_wait: tokio::sync::watch::channel(false).0,
                },
                body,
                permit,
                |_| Ok(json!({})),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while sender.capacity() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), catalog.close())
        .await
        .unwrap()
        .unwrap();
    assert!(request.await.unwrap().is_err());
    assert!(sender.is_closed());
}

#[tokio::test]
async fn ram_resumable_eof_cancellation_keeps_the_owned_checkpoint_offset_and_hash_together() {
    let catalog = setup(200_000, false).await;
    let active = session(&catalog).await;
    chunk(&catalog, &active.id, b"part", 0, None, false)
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&active.id, "test-bucket")
        .await
        .unwrap();
    let options = lease.session.options.clone();
    let permit = catalog.admit_object_upload().unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let worker = catalog.clone();
    let gate = tokio::spawn(async move {
        worker
            .run_object(move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"tail"))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    drop(sender);
    let (catalog_wait, mut progress) = tokio::sync::watch::channel(false);
    let worker = catalog.clone();
    let request = tokio::spawn(async move {
        worker
            .write_resumable_session(
                lease,
                ResumableChunk {
                    start: 4,
                    length: 4,
                    total: None,
                    finalize: false,
                    open_end: false,
                    options,
                    catalog_wait,
                },
                body,
                permit,
                |_| Ok(json!({})),
            )
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !*progress.borrow_and_update() {
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    release.send(()).unwrap();
    gate.await.unwrap().unwrap();
    let lease = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if let Ok(lease) = catalog
                .lock_resumable_session(&active.id, "test-bucket")
                .await
            {
                break lease;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(lease.session.offset, 8);
    drop(lease);
    chunk(&catalog, &active.id, b"", 8, Some(8), true)
        .await
        .unwrap();
    let result = catalog
        .get_object("test-bucket", "value", None, Default::default())
        .await
        .unwrap();
    assert_eq!(
        (result.md5, result.crc32c),
        (expected().md5, expected().crc32c)
    );
    catalog.close().await.unwrap();
}
