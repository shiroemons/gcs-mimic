use super::*;
use gcs_mimic::storage::{ResumableChunk, ResumableOptions, ResumableSession, SessionState};

async fn create(catalog: &Catalog, clock: &TestClock) -> Result<ResumableSession, ResumableError> {
    // Distinct initiation times give eviction a deterministic oldest-first order.
    *clock.0.lock().unwrap() += Duration::from_millis(1);
    catalog
        .create_resumable_session(
            "bucket",
            "name",
            ResumableOptions::new(gcs_mimic::bucket::BucketAccess::Permissive),
            None,
        )
        .await
}

async fn complete(catalog: &Catalog, id: &str) -> serde_json::Value {
    let lease = catalog.lock_resumable_session(id, "bucket").await.unwrap();
    let options = lease.session.options.clone();
    let (sender, body) = tokio::sync::mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(hyper::body::Bytes::from_static(b"x"))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    drop(sender);
    catalog
        .write_resumable_session(
            lease,
            ResumableChunk {
                start: 0,
                length: 1,
                total: Some(1),
                finalize: true,
                open_end: false,
                options,
                catalog_wait: tokio::sync::watch::channel(false).0,
            },
            body,
            catalog.admit_object_upload().unwrap(),
            |record| Ok(serde_json::json!({"generation": record.generation})),
        )
        .await
        .unwrap()
        .unwrap()
}

fn session_counts(dir: &TestDir) -> (i64, i64) {
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT coalesce(sum(state = 'ACTIVE'), 0), coalesce(sum(state != 'ACTIVE'), 0) FROM resumable_sessions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

#[tokio::test]
async fn completed_sessions_do_not_block_new_uploads_and_retained_results_stay_bounded() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let mut completed = Vec::new();
    for _ in 0..130 {
        let session = create(&catalog, &clock).await.unwrap();
        let resource = complete(&catalog, &session.id).await;
        completed.push((session.id, resource));
    }
    // Only the oldest result beyond the retained-result cap is reclaimed; recent completions
    // still answer status and repeated final requests with their original resource.
    assert_eq!(
        catalog
            .resumable_session(&completed[0].0, "bucket")
            .await
            .unwrap_err(),
        ResumableError::NotFound
    );
    for (id, resource) in &completed[1..] {
        let session = catalog.resumable_session(id, "bucket").await.unwrap();
        assert_eq!(session.state, SessionState::Complete);
        assert_eq!(session.completion.as_ref(), Some(resource));
    }
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);

    // Active sessions keep their own cap, and admission trims results to make the total bounded.
    let mut active = Vec::new();
    for _ in 0..128 {
        active.push(create(&catalog, &clock).await.unwrap());
    }
    assert!(matches!(
        create(&catalog, &clock).await.unwrap_err(),
        ResumableError::Object(error) if error.to_string().contains("retained upload session limit reached")
    ));
    assert_eq!(session_counts(&dir), (128, 128));
    assert_eq!(
        std::fs::read_dir(dir.0.join("staging")).unwrap().count(),
        128
    );

    // A cancelled session releases its active slot once its staged bytes are reclaimed.
    assert_eq!(cancel_wire(&catalog, &active[0].id, b"").await, 499);
    let replacement = create(&catalog, &clock).await.unwrap();
    assert_eq!(cancel_wire(&catalog, &active[0].id, b"").await, 410);
    catalog.close().await.unwrap();

    // The doubled bound is a valid catalog state across restart.
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    assert_eq!(session_counts(&dir), (128, 128));
    let last = completed.last().unwrap();
    assert_eq!(
        catalog
            .resumable_session(&last.0, "bucket")
            .await
            .unwrap()
            .completion
            .as_ref(),
        Some(&last.1)
    );
    complete(&catalog, &replacement.id).await;
    assert_eq!(session_counts(&dir), (127, 129));
    create(&catalog, &clock).await.unwrap();
    assert_eq!(session_counts(&dir), (128, 128));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_sessions_awaiting_cleanup_are_kept_and_hold_the_terminal_cap() {
    let dir = TestDir::new();
    let clock = Arc::new(TestClock(Mutex::new(UNIX_EPOCH)));
    let catalog = setup(&dir, clock.clone()).await;
    let mut cancelled = Vec::new();
    for _ in 0..129 {
        let session = create(&catalog, &clock).await.unwrap();
        // A directory in place of the staging file makes cleanup fail and keep its journal.
        let stage = dir.0.join("staging").join(&session.blob_id);
        std::fs::remove_file(&stage).unwrap();
        std::fs::create_dir(&stage).unwrap();
        assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 499);
        cancelled.push(session.id);
    }
    assert_eq!(session_counts(&dir), (0, 129));
    // Trimming would drop the record maintenance needs to finish cleanup, so admission refuses
    // rather than growing past the bound.
    assert!(matches!(
        create(&catalog, &clock).await.unwrap_err(),
        ResumableError::Object(error) if error.to_string().contains("retained upload session limit reached")
    ));
    assert_eq!(session_counts(&dir), (0, 129));
    for id in &cancelled {
        assert_eq!(cancel_wire(&catalog, id, b"").await, 410);
    }
    catalog.close().await.unwrap();
}
