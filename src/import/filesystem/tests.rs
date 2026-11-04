use super::*;
use crate::storage::{change_journal::ChangeKind, failure::unavailable};

fn interrupted_import() -> (crate::test_support::TestDir, Options) {
    let dir = crate::test_support::TestDir::new();
    let source = dir.0.join("source");
    fs::create_dir_all(source.join("bucket-one")).unwrap();
    let options = Options::new(
        source,
        dir.0.join("destination"),
        super::super::MetadataMode::Sidecar,
        super::super::SourcePlatform::Unix,
    );
    assert!(
        execute(
            options.clone(),
            super::super::test_support::StopAfterLines(1),
            Vec::new()
        )
        .is_err()
    );
    (dir, options)
}

#[test]
fn missing_import_lock_resumes_or_explains_how_to_restore_it() {
    let (_dir, options) = interrupted_import();
    fs::remove_file(options.destination.join("catalog.lock")).unwrap();
    #[cfg(unix)]
    {
        assert!(
            execute(options.clone(), Vec::new(), Vec::new())
                .unwrap()
                .complete
        );
        assert!(options.destination.join("catalog.lock").is_file());
    }
    #[cfg(not(unix))]
    {
        let before = fs::read(options.destination.join("catalog.sqlite3")).unwrap();
        let error = execute(options.clone(), Vec::new(), Vec::new()).unwrap_err();
        assert!(error.to_string().contains("restore catalog.lock"));
        let ImportError::Io(error) = error else {
            panic!("expected invalid import input")
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(matches!(
            error.get_ref().unwrap().downcast_ref::<ResumeError>(),
            Some(ResumeError::MissingLock)
        ));
        assert_eq!(
            fs::read(options.destination.join("catalog.sqlite3")).unwrap(),
            before
        );
        assert!(!options.destination.join("catalog.lock").exists());
    }
}

#[test]
fn permanently_failed_import_resume_preserves_the_invalid_data_error() {
    let (_dir, options) = interrupted_import();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        let journal = catalog.import_journal();
        let state = journal.progress().await.unwrap().unwrap();
        assert!(state.active);
        journal
            .fail(state.binding, "checksum mismatch")
            .await
            .unwrap();
        catalog.close().await.unwrap();
    });
    let before = fs::read(options.destination.join("catalog.sqlite3")).unwrap();
    let error = execute(options.clone(), Vec::new(), Vec::new()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "import permanently failed: checksum mismatch; preserve the source snapshot and restart into a new empty destination"
    );
    let ImportError::Io(error) = error else {
        panic!("expected invalid import input")
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(
        matches!(error.get_ref().unwrap().downcast_ref::<ResumeError>(), Some(ResumeError::PermanentlyFailed { reason }) if reason == "checksum mismatch")
    );
    assert_eq!(
        fs::read(options.destination.join("catalog.sqlite3")).unwrap(),
        before
    );
}

#[test]
fn import_refuses_an_unbound_catalog_without_changing_destination_bytes() {
    for lock_present in [true, false] {
        let dir = crate::test_support::TestDir::new();
        let source = dir.0.join("source");
        fs::create_dir(&source).unwrap();
        let destination = dir.0.join("destination");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let catalog = Catalog::open(&destination).await.unwrap();
            crate::storage::test_fixtures::seed_bucket(&catalog).await;
            catalog.close().await.unwrap();
        });
        if !lock_present {
            fs::remove_file(destination.join("catalog.lock")).unwrap();
        }
        let stage = destination
            .join("staging")
            .join("00000000000000000000000000000001");
        fs::write(&stage, b"preserve this orphan until server recovery").unwrap();
        let before = fs::read(destination.join("catalog.sqlite3")).unwrap();
        let names = || {
            let mut names: Vec<_> = fs::read_dir(&destination)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            names
        };
        let before_names = names();
        let options = Options::new(
            source,
            destination.clone(),
            super::super::MetadataMode::Sidecar,
            super::super::SourcePlatform::Unix,
        );
        let error = execute(options, Vec::new(), Vec::new()).unwrap_err();
        #[cfg(unix)]
        assert!(
            error
                .to_string()
                .starts_with("data-dir: destination is not the same unfinished import"),
            "{error}"
        );
        #[cfg(not(unix))]
        if !lock_present {
            assert!(
                error.to_string().contains("restore catalog.lock"),
                "{error}"
            );
        }
        assert_eq!(
            fs::read(destination.join("catalog.sqlite3")).unwrap(),
            before
        );
        assert_eq!(names(), before_names);
        assert_eq!(
            fs::read(&stage).unwrap(),
            b"preserve this orphan until server recovery"
        );
    }
}

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
