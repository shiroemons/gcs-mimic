use super::*;
use crate::{
    import::spec::{BucketSpec, ObjectSpec, SourceHashes},
    storage::{admin::Generation, change_journal::ChangeKind, failure::unavailable},
};

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
        assert!(
            matches!(error, ImportError::Resume(ResumeError::MissingLock)),
            "{error:?}"
        );
        assert_eq!(
            fs::read(options.destination.join("catalog.sqlite3")).unwrap(),
            before
        );
        assert!(!options.destination.join("catalog.lock").exists());
    }
}

#[test]
fn permanently_failed_import_resume_reports_the_journal_refusal() {
    let (_dir, options) = interrupted_import();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        let journal = catalog.import_journal();
        let state = journal.progress().await.unwrap().unwrap();
        assert!(state.is_open());
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
    assert!(
        matches!(&error, ImportError::Resume(ResumeError::PermanentlyFailed { reason }) if reason.as_str() == "checksum mismatch"),
        "{error:?}"
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
    let record = bucket_record("bucket", None, [0; 32]);
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

/// A bucket record whose digest inputs are the given path (also its name) and attributes digest.
fn bucket_record(name: &str, created_timestamp: Option<&str>, digest: [u8; 32]) -> Record {
    Record::Bucket(Box::new(BucketRecord {
        path: name.into(),
        name: name.parse().unwrap(),
        attributes: Default::default(),
        created_timestamp: created_timestamp.map(Into::into),
        enable_object_retention: created_timestamp.is_some(),
        generated: &[],
        observed_stat: "ignored".into(),
        attributes_digest: digest,
        spec: BucketSpec {
            created: created_timestamp.map(|time| codec::parse_bucket_timestamp(time).unwrap()),
            acl: None,
        },
    }))
}

/// A resume compares each committed element's journal digest with the one its source has now, so
/// the digest keeps the bytes earlier releases wrote: a change here strands every unfinished
/// import they started.
#[test]
fn element_digests_keep_the_bytes_earlier_journals_hold() {
    let bucket = bucket_record(
        "bucket-one",
        Some("2026-01-01T00:00:00.123456789Z"),
        [1; 32],
    );
    let object = Record::Object(Box::new(ObjectRecord {
        path: "bucket-one/folder/body".into(),
        bucket: "bucket-one".parse().unwrap(),
        name: "folder/body".parse().unwrap(),
        attributes: Default::default(),
        created_micros: -1,
        updated_micros: 1_792_800_000_123_456,
        deleted_micros: None,
        fingerprint: super::super::Fingerprint {
            size: 5,
            mtime_ns: 1_700_000_000_123_456_700,
            ctime_ns: Some(1_700_000_001_000_000_001),
            inode: Some(42),
            device: Some(7),
        },
        verified_hashes: None,
        generated: &[],
        source_file: None,
        attributes_digest: [2; 32],
        spec: ObjectSpec {
            generation: Generation::new(1_792_800_000_123_456).unwrap(),
            hashes: SourceHashes {
                md5: [3; 16],
                crc32c: 4,
            },
            storage_class: crate::bucket::StorageClass::Standard,
            acl: None,
            retention: None,
            custom_time: None,
        },
    }));
    let element = |record: &Record, ordinal| {
        let element = super::element(record, ordinal).unwrap();
        (
            crate::hashing::encode_lower(&element.fingerprint),
            element.ordinal,
            element.inode,
            element.ctime_ns,
        )
    };
    assert_eq!(
        element(&bucket, 1),
        (
            "81624196e74d0f5e8ebb86b08322960ffd6b63dd673970fe218ffade1a3a14ef".into(),
            1,
            None,
            None
        )
    );
    assert_eq!(
        element(&object, 2),
        (
            "03db7e12daff751c2c14c3be0355e8b2025f382ab67e6ef48f15f6f41f107082".into(),
            2,
            Some(42),
            Some(1_700_000_001_000_000_001)
        )
    );
    assert!(super::element(&Record::Directory { path: "x".into() }, 3).is_none());
}

/// A refused scan fails as its most severe blocker, whatever order the findings came in: source
/// I/O failures and stat drift alone are retried, and any finding that needs the source repaired
/// makes the failure terminal. The journal records the text of a terminal failure.
#[test]
fn a_refused_scan_fails_as_its_most_severe_blocker() {
    const UNREADABLE: &str = "source I/O failure: resolve the reported filesystem error and retry the same immutable snapshot";
    const DRIFT: &str = "source_changed: [Inode, Ctime]";
    const BLOCKED: &str =
        "import preflight found blockers; restore an immutable supported snapshot and retry";
    for (blockers, text, is_terminal) in [
        (&[Blocker::Io][..], UNREADABLE, false),
        (&[Blocker::Io, Blocker::Io], UNREADABLE, false),
        (&[Blocker::StatDrift], DRIFT, false),
        (&[Blocker::StatDrift, Blocker::StatDrift], DRIFT, false),
        (&[Blocker::Io, Blocker::StatDrift], UNREADABLE, false),
        (&[Blocker::StatDrift, Blocker::Io], UNREADABLE, false),
        (&[Blocker::Source], BLOCKED, true),
        (&[Blocker::StatDrift, Blocker::Source], BLOCKED, true),
        (&[Blocker::Source, Blocker::Io], BLOCKED, true),
        (
            &[Blocker::Io, Blocker::Source, Blocker::StatDrift],
            BLOCKED,
            true,
        ),
        (&[], BLOCKED, true),
    ] {
        let mut summary = Summary::default();
        for blocker in blockers {
            summary.block(*blocker);
        }
        assert_eq!(summary.blocking_findings, blockers.len() as u64);
        let error = require_eligible(&summary).unwrap_err();
        assert_eq!(error.to_string(), text, "{blockers:?}");
        assert_eq!(terminal(&error), is_terminal, "{blockers:?}");
        let drift = matches!(
            &error,
            ImportError::Source(failure) if failure.retryable_stat_drift()
        );
        assert_eq!(drift, text == DRIFT, "{blockers:?}");
        summary.eligible = true;
        assert!(require_eligible(&summary).is_ok(), "{blockers:?}");
    }
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
