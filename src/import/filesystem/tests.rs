use super::*;
use crate::storage::{change_journal::ChangeKind, failure::unavailable};

#[test]
fn buffered_observations_are_anonymous_and_report_the_last_flush_failure() {
    let file = Baseline::create().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(file.metadata().unwrap().nlink(), 0);
    }
    let record = Record::Bucket {
        path: "bucket".into(),
        name: "bucket".into(),
        attributes: Default::default(),
        created_timestamp: None,
        enable_object_retention: false,
        generated: &[],
        observed_stat: String::new(),
        attributes_digest: [0; 32],
    };
    let mut recorder = Baseline::open(file.try_clone().unwrap(), BaselineMode::Record).unwrap();
    recorder.check(&record).unwrap();
    recorder.finish().unwrap();
    let mut verifier = Baseline::open(file.try_clone().unwrap(), BaselineMode::Verify).unwrap();
    verifier.check(&record).unwrap();
    assert!(matches!(
        verifier.check(&record),
        Err(ImportFailure::SourceChanged { .. })
    ));
    let directory = crate::test_support::TestDir::new();
    let path = directory.0.join("readonly");
    fs::write(&path, b"").unwrap();
    let mut failing = Baseline {
        file: BaselineIo::Record(BufWriter::new(File::open(path).unwrap())),
    };
    failing.check(&record).unwrap();
    assert!(failing.finish().is_err());
}

#[tokio::test]
async fn joining_a_scanner_does_not_block_the_async_worker() {
    let (release, blocked) = std::sync::mpsc::channel();
    let (send, worker) = tokio::sync::oneshot::channel();
    crate::task::spawn_thread("import-join-test", move || {
        blocked.recv_timeout(Duration::from_secs(1)).unwrap();
        send.send(Ok(Summary::default())).unwrap();
    })
    .unwrap();
    let join = join_scan(worker);
    tokio::pin!(join);
    tokio::select! {
        biased;
        result = &mut join => panic!("scanner completed before release: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(10)) => {},
    }
    release.send(()).unwrap();
    join.await.unwrap().unwrap();
}

#[test]
fn unknown_stored_values_make_import_failures_terminal() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let error = connection
        .query_row("SELECT 'unknown'", [], |row| row.get::<_, ChangeKind>(0))
        .unwrap_err();
    let error: AdminError = unavailable(error).into();
    assert!(matches!(error, AdminError::Corrupt(_)));
    assert!(terminal(&error.into()));
    let query_error: AdminError = unavailable(rusqlite::Error::InvalidQuery).into();
    assert!(!terminal(&query_error.into()));
}
