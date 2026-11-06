use super::*;
use gcs_mimic::{
    config::Config,
    storage::{ResumableChunk, ResumableOptions, ResumableSession, SessionState},
};
use std::num::NonZeroU32;

async fn create(
    catalog: &Catalog,
    clock: &ManualClock,
) -> Result<ResumableSession, ResumableError> {
    // Distinct initiation times give eviction a deterministic oldest-first order.
    clock.advance(Duration::from_millis(1));
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
    let body = body(vec![hyper::body::Bytes::from_static(b"x")]);
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

/// Past the 128 active and 128 finished sessions that once failed requests and refused to open.
#[tokio::test]
async fn sessions_past_the_former_caps_survive_reopening_and_stay_usable() {
    let dir = TestDir::new();
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
    let catalog = setup(&dir, clock.clone()).await;
    let mut completed = Vec::new();
    for _ in 0..130 {
        let session = create(&catalog, &clock).await.unwrap();
        let resource = complete(&catalog, &session.id).await;
        completed.push((session.id, resource));
    }
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    let mut active = Vec::new();
    for _ in 0..300 {
        active.push(create(&catalog, &clock).await.unwrap());
    }
    assert_eq!(session_counts(&dir), (300, 130));
    assert_eq!(
        std::fs::read_dir(dir.0.join("staging")).unwrap().count(),
        300
    );
    catalog.close().await.unwrap();

    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    assert_eq!(session_counts(&dir), (300, 130));
    // Finished sessions still answer status and repeated final requests with their resource.
    for (id, resource) in &completed {
        let session = catalog.resumable_session(id, "bucket").await.unwrap();
        assert_eq!(session.state, SessionState::Complete);
        assert_eq!(session.completion.as_ref(), Some(resource));
    }
    // Every active session is still known, and one past the old cap can finish.
    for session in &active {
        let found = catalog
            .resumable_session(&session.id, "bucket")
            .await
            .unwrap();
        assert_eq!(found.state, SessionState::Active);
    }
    complete(&catalog, &active[299].id).await;
    assert_eq!(cancel_wire(&catalog, &active[0].id, b"").await, 499);
    create(&catalog, &clock).await.unwrap();
    assert_eq!(session_counts(&dir), (299, 132));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_configured_cap_applies_only_to_active_sessions() {
    let dir = TestDir::new();
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
    let catalog = setup(&dir, clock.clone()).await;
    catalog.apply_session_limits(&Config {
        max_resumable_sessions: NonZeroU32::new(2),
        ..Default::default()
    });
    // Finished records never count against it, or every upload would fail once two had finished.
    for _ in 0..3 {
        let session = create(&catalog, &clock).await.unwrap();
        complete(&catalog, &session.id).await;
    }
    let first = create(&catalog, &clock).await.unwrap();
    create(&catalog, &clock).await.unwrap();
    assert!(matches!(
        create(&catalog, &clock).await.unwrap_err(),
        ResumableError::Object(error) if error.to_string().contains("retained upload session limit reached")
    ));
    assert_eq!(session_counts(&dir), (2, 3));
    // A cancelled session releases its active slot once its staged bytes are reclaimed.
    assert_eq!(cancel_wire(&catalog, &first.id, b"").await, 499);
    create(&catalog, &clock).await.unwrap();
    assert_eq!(cancel_wire(&catalog, &first.id, b"").await, 499);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn cancelled_sessions_awaiting_cleanup_keep_answering_while_new_uploads_proceed() {
    let dir = TestDir::new();
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
    let catalog = setup(&dir, clock.clone()).await;
    let mut cancelled = Vec::new();
    for _ in 0..3 {
        let session = create(&catalog, &clock).await.unwrap();
        // A directory in place of the staging file makes cleanup fail and keep its journal.
        let stage = session.blob_id.staging_path(&dir.0);
        std::fs::remove_file(&stage).unwrap();
        std::fs::create_dir(&stage).unwrap();
        assert_eq!(cancel_wire(&catalog, &session.id, b"").await, 499);
        cancelled.push(session.id);
    }
    create(&catalog, &clock).await.unwrap();
    assert_eq!(session_counts(&dir), (1, 3));
    for id in &cancelled {
        assert_eq!(cancel_wire(&catalog, id, b"").await, 499);
    }
    catalog.close().await.unwrap();
}
