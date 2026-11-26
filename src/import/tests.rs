use super::*;
use crate::{
    fsutil::{SourceMetadataBuffer, SourceMetadataError},
    test_support::TestDir,
};
use std::fs::File;

#[test]
fn import_value_flag_spellings_are_exact() {
    for (flag, spelling) in [
        (ValueFlag::Source, "--source"),
        (ValueFlag::DataDir, "--data-dir"),
        (ValueFlag::MetadataMode, "--metadata-mode"),
        (ValueFlag::SourcePlatform, "--source-platform"),
        (ValueFlag::SourceFormat, "--source-format"),
        (ValueFlag::MaxObjectBytes, "--max-object-bytes"),
        (ValueFlag::MaxEntries, "--max-entries"),
        (ValueFlag::MaxDepth, "--max-depth"),
    ] {
        assert_eq!(flag.as_str(), spelling);
        assert_eq!(ValueFlag::from_flag(spelling), Some(flag));
    }
    for rejected in ["source", "--Source", "--source=", "--dry-run"] {
        assert_eq!(ValueFlag::from_flag(rejected), None);
    }
}

#[test]
fn a_catalog_failure_keeps_its_cli_diagnostic_and_sqlite_source() {
    use std::error::Error;
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some("malformed stored row".into()),
    );
    let error = crate::bucket::CatalogError::from(sqlite);
    assert_eq!(error.to_string(), "catalog corrupt: malformed stored row");
    assert!(error.source().unwrap().is::<rusqlite::Error>());
    let diagnostics = execution_diagnostic(&ImportError::Catalog(error));
    assert_eq!(
        diagnostics.errors[0].fix,
        "Import failed: catalog corrupt: malformed stored row. Keep the source snapshot unchanged; resolve the reported cause and retry only the same source/options. Terminal failures require a new empty destination; never remove only the import marker."
    );
}

fn xattr_setup() -> (TestDir, Options) {
    let directory = TestDir::new();
    let source = directory.0.join("source");
    fs::create_dir_all(source.join("bucket-one")).unwrap();
    let options = Options::new(
        source,
        directory.0.join("destination"),
        MetadataMode::Sidecar,
        SourcePlatform::Unix,
    );
    (directory, options)
}

#[tokio::test]
async fn scanner_panics_arrive_as_typed_results() {
    fn fail<'a>(
        _: &File,
        _: &'a mut SourceMetadataBuffer,
    ) -> Result<Option<&'a [u8]>, SourceMetadataError> {
        panic!("injected scanner panic")
    }
    let (_dir, mut options) = xattr_setup();
    fs::write(options.source.join("bucket-one/body"), b"body").unwrap();
    options.metadata_mode = MetadataMode::Xattr;
    let (mut pages, completion) =
        scan_with_baseline(options, fail, None, ScanPurpose::Inspect).unwrap();
    while pages.recv().await.is_some() {}
    let error = completion.await.unwrap().unwrap_err();
    assert_eq!(error.to_string(), "source I/O failure (Other)");
    assert!(
        crate::error::causes(&error)
            .any(|cause| cause.to_string().contains("import scanner panicked"))
    );
}
#[cfg(not(unix))]
#[test]
fn snapshot_import_is_rejected_before_reading_on_non_unix_hosts() {
    let (directory, mut options) = xattr_setup();
    options.source_format = SourceFormat::Snapshot;
    let error = inspect(options.clone(), Vec::new(), Vec::new())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(
            "--source-format: Memory snapshot import is supported on Linux and macOS only"
        ),
        "{error}"
    );
    assert!(!options.destination.exists());
    drop(directory);
}

fn xattr_report(options: Options) -> (Summary, Vec<serde_json::Value>) {
    let mut output = Vec::new();
    let summary = inspect(options, &mut output, Vec::new()).unwrap();
    let pages = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (summary, pages)
}
fn xattr_finding(pages: &[serde_json::Value], key: &str, reason: &str) {
    assert_eq!(
        pages
            .iter()
            .filter_map(|p| p["page"]["findings"].as_array())
            .flatten()
            .filter(
                |f| f["diagnostic"]["key"] == key && f["reason"].as_str().unwrap().contains(reason)
            )
            .count(),
        1
    );
}
const ATTRS: &[u8] = include_bytes!("../../tests/fixtures/import/object-attrs.json");

#[test]
fn filesystem_import_preserves_explicit_bucket_and_object_acls_after_restart() {
    let (_dir, options) = xattr_setup();
    fs::write(
        options.source.join("bucket-one.bucketMetadata"),
        br#"{"ACL":[{"Entity":"allAuthenticatedUsers","Role":"WRITER"}]}"#,
    )
    .unwrap();
    for (name, acl) in [
        (
            "public",
            serde_json::json!([{"Entity":"allUsers","Role":"READER",
            "EntityID":"","Domain":"","Email":"","ProjectTeam":null}]),
        ),
        ("empty", serde_json::json!([])),
    ] {
        let mut attrs: serde_json::Value = serde_json::from_slice(ATTRS).unwrap();
        attrs["ACL"] = acl;
        fs::write(options.source.join("bucket-one").join(name), b"hello").unwrap();
        fs::write(
            options
                .source
                .join("bucket-one")
                .join(format!("{name}.metadata")),
            serde_json::to_vec(&attrs).unwrap(),
        )
        .unwrap();
    }
    assert!(
        execute(options.clone(), Vec::new(), Vec::new())
            .unwrap()
            .complete
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for _ in 0..2 {
        runtime.block_on(async {
            let catalog = crate::storage::Catalog::open(&options.destination)
                .await
                .unwrap();
            let projection = catalog
                .acl_projection(
                    "bucket-one",
                    vec![
                        ("empty".into(), 1_792_800_000_123_456),
                        ("public".into(), 1_792_800_000_123_456),
                    ],
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                projection.bucket,
                vec![crate::acl::AclEntry {
                    entity: "allAuthenticatedUsers".parse().unwrap(),
                    role: crate::acl::AclRole::Writer
                }]
            );
            assert!(projection.objects[0].entries().is_empty());
            assert_eq!(
                projection.objects[1].entries(),
                vec![crate::acl::AclEntry {
                    entity: "allUsers".parse().unwrap(),
                    role: crate::acl::AclRole::Reader
                }]
            );
            catalog.close().await.unwrap();
        });
    }
    assert_eq!(
        fs::read(options.source.join("bucket-one/public")).unwrap(),
        b"hello"
    );
}

#[test]
fn source_acl_validation_rejects_unrepresentable_identity_and_ambiguous_grants() {
    for acl in [
        serde_json::json!([{"Entity":"allUsers","Role":"WRITER"}]),
        serde_json::json!([{"Entity":"allUsers","Role":"READER","Email":"x@example.invalid"}]),
        serde_json::json!([{"Entity":"allUsers","Role":"READER","Unknown":true}]),
        serde_json::json!([{"Entity":"allUsers","Role":"READER"},{"Entity":"allUsers","Role":"OWNER"}]),
    ] {
        let (_dir, options) = xattr_setup();
        let mut attrs: serde_json::Value = serde_json::from_slice(ATTRS).unwrap();
        attrs["ACL"] = acl;
        fs::write(options.source.join("bucket-one/body"), b"hello").unwrap();
        fs::write(
            options.source.join("bucket-one/body.metadata"),
            serde_json::to_vec(&attrs).unwrap(),
        )
        .unwrap();
        let (summary, pages) = xattr_report(options.clone());
        assert!(!summary.eligible);
        assert_eq!(summary.blocking_findings, 1, "{pages:?}");
        assert!(execute(options.clone(), Vec::new(), Vec::new()).is_err());
        assert!(!options.destination.exists());
    }
}

#[test]
fn filesystem_import_xattr_revalidation_preserves_source_metadata() {
    let (dir, mut options) = xattr_setup();
    options.metadata_mode = MetadataMode::Xattr;
    let path = options.source.join("bucket-one/body");
    fs::write(&path, b"hello").unwrap();
    let file = fs::File::open(&path).unwrap();
    if !crate::fsutil::test_support::set_metadata(&file, ATTRS).unwrap() {
        eprintln!(
            "xattr import execution unavailable: source filesystem does not support user.metadata"
        );
        return;
    }
    assert!(
        execute(
            options.clone(),
            super::test_support::StopAfterLines(2),
            Vec::new()
        )
        .is_err()
    );
    // Identical xattr writes need not change ctime; a replacement inode guarantees drift.
    let original = file.metadata().unwrap();
    let replacement = dir.0.join("replacement");
    fs::write(&replacement, b"hello").unwrap();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&replacement)
        .unwrap();
    assert!(crate::fsutil::test_support::set_metadata(&file, ATTRS).unwrap());
    file.set_times(std::fs::FileTimes::new().set_modified(original.modified().unwrap()))
        .unwrap();
    fs::rename(replacement, &path).unwrap();
    let before = Fingerprint::of(&file.metadata().unwrap()).unwrap();
    assert_ne!(before.inode, Fingerprint::of(&original).unwrap().inode);
    let mut diagnostics = Vec::new();
    let summary = execute(options, Vec::new(), &mut diagnostics).unwrap();
    assert!(summary.bodies_verified);
    assert!(
        String::from_utf8(diagnostics)
            .unwrap()
            .contains("stat_identity_drift")
    );
    assert_eq!(before, Fingerprint::of(&file.metadata().unwrap()).unwrap());
    assert_eq!(
        crate::fsutil::read_source_metadata(&file, &mut SourceMetadataBuffer::default()).unwrap(),
        Some(ATTRS)
    );
}

#[test]
fn xattr_snapshot_verifies_bodies_without_mutating_source_attributes() {
    use crate::fsutil::{SourceMetadataBuffer, read_source_metadata};
    let (_dir, mut options) = xattr_setup();
    options.metadata_mode = MetadataMode::Xattr;
    options.verify_bodies = true;
    let path = options.source.join("bucket-one/body");
    fs::write(&path, b"hello").unwrap();
    let file = fs::File::open(&path).unwrap();
    if !crate::fsutil::test_support::set_metadata(&file, ATTRS).unwrap() {
        eprintln!(
            "xattr import execution unavailable: source filesystem does not support user.metadata"
        );
        return;
    }
    let before = Fingerprint::of(&file.metadata().unwrap()).unwrap();
    let (summary, _) = xattr_report(options.clone());
    assert!(summary.eligible && summary.bodies_verified);
    assert_eq!(summary.bytes_hashed, 5);
    assert_eq!(before, Fingerprint::of(&file.metadata().unwrap()).unwrap());
    assert_eq!(
        read_source_metadata(&file, &mut SourceMetadataBuffer::default()).unwrap(),
        Some(ATTRS)
    );
    assert_eq!(fs::read(&path).unwrap(), b"hello");
    options.metadata_mode = MetadataMode::Sidecar;
    fs::write(options.source.join("bucket-one/body.metadata"), ATTRS).unwrap();
    let (summary, pages) = xattr_report(options);
    assert!(!summary.eligible);
    xattr_finding(
        &pages,
        "bucket-one/body:user.metadata",
        "mixed metadata modes",
    );

    let (_dir, mut missing) = xattr_setup();
    missing.metadata_mode = MetadataMode::Xattr;
    fs::write(missing.source.join("bucket-one/missing"), b"hello").unwrap();
    let (summary, pages) = xattr_report(missing);
    assert_eq!(summary.blocking_findings, 1);
    xattr_finding(
        &pages,
        "bucket-one/missing:user.metadata",
        "missing user.metadata",
    );
}

fn unsupported<'a>(
    _: &File,
    _: &'a mut SourceMetadataBuffer,
) -> Result<Option<&'a [u8]>, SourceMetadataError> {
    Err(SourceMetadataError::Unsupported)
}

#[test]
fn unsupported_xattr_inspection_warns_once_per_sidecar_object() {
    let directory = TestDir::new();
    let source = directory.0.join("source");
    fs::create_dir_all(source.join("bucket-one")).unwrap();
    fs::write(source.join("bucket-one/body"), b"hello").unwrap();
    fs::write(
        source.join("bucket-one/body.metadata"),
        include_bytes!("../../tests/fixtures/import/object-attrs.json"),
    )
    .unwrap();
    let options = Options::new(
        source,
        directory.0.join("destination"),
        MetadataMode::Sidecar,
        SourcePlatform::Unix,
    );
    let mut output = Vec::new();
    let summary = inspect_with_xattr(options, &mut output, Vec::new(), unsupported).unwrap();
    assert!(summary.eligible && !summary.mixed_format_absence_verified);
    assert_eq!(summary.warnings, 1);
    let lines: Vec<serde_json::Value> = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let findings: Vec<_> = lines
        .iter()
        .filter_map(|p| p["page"]["findings"].as_array())
        .flatten()
        .collect();
    assert_eq!(findings.len(), 1);
    assert_eq!(
        findings[0]["diagnostic"]["key"],
        "bucket-one/body:user.metadata"
    );
    assert!(
        findings[0]["reason"]
            .as_str()
            .unwrap()
            .contains("absence could not be verified")
    );
}

#[test]
fn ordinary_xattr_objects_do_not_get_a_detritus_probe() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static READS: AtomicUsize = AtomicUsize::new(0);
    fn counting<'a>(
        file: &File,
        buffer: &'a mut SourceMetadataBuffer,
    ) -> Result<Option<&'a [u8]>, SourceMetadataError> {
        READS.fetch_add(1, Ordering::SeqCst);
        crate::fsutil::read_source_metadata(file, buffer)
    }
    let directory = TestDir::new();
    let source = directory.0.join("source");
    fs::create_dir_all(source.join("bucket-one")).unwrap();
    let body = source.join("bucket-one/body");
    fs::write(&body, b"hello").unwrap();
    if !crate::fsutil::test_support::set_metadata(
        &File::open(body).unwrap(),
        include_bytes!("../../tests/fixtures/import/object-attrs.json"),
    )
    .unwrap()
    {
        eprintln!(
            "xattr call-count execution unavailable: source filesystem does not support user.metadata"
        );
        return;
    }
    let options = Options::new(
        source,
        directory.0.join("destination"),
        MetadataMode::Xattr,
        SourcePlatform::Unix,
    );
    let summary = inspect_with_xattr(options, Vec::new(), Vec::new(), counting).unwrap();
    assert!(summary.eligible);
    assert_eq!(summary.bytes_hashed, 0);
    assert_eq!(READS.load(Ordering::SeqCst), 2);
}

/// An unfinished import's identity includes its limits. The bytes an earlier release recorded for
/// its defaults have to stay reachable, or an upgrade would refuse to resume what it started.
#[test]
fn an_import_identity_tells_no_limit_from_the_former_defaults_and_can_reach_both() {
    let options = Options::new(
        "source".into(),
        "destination".into(),
        MetadataMode::Sidecar,
        SourcePlatform::Unix,
    );
    let fields = option_fields(&options);
    assert_eq!(fields[3], MAX_OBJECT_BYTES.to_be_bytes());
    assert_eq!(fields[4], 0u64.to_be_bytes());
    assert_eq!(fields[5], 0u64.to_be_bytes());
    let former = options.with_former_defaults().unwrap();
    let fields = option_fields(&former);
    assert_eq!(fields[3], 1_073_741_824u64.to_be_bytes());
    assert_eq!(fields[4], 10_000u64.to_be_bytes());
    assert_eq!(fields[5], 32u64.to_be_bytes());
    assert!(former.with_former_defaults().is_none());
}

/// A source whose digests the tests below pin: one bucket with attributes, a directory, and an
/// object whose body has a modification time every platform holds exactly.
fn digest_fixture() -> (TestDir, Options, std::path::PathBuf) {
    let directory = TestDir::new();
    let source = directory.0.join("source");
    fs::create_dir_all(source.join("bucket-one/folder")).unwrap();
    fs::write(
        source.join("bucket-one.bucketMetadata"),
        br#"{"DefaultEventBasedHold":true}"#,
    )
    .unwrap();
    let body = source.join("bucket-one/folder/body");
    fs::write(&body, b"hello").unwrap();
    // The manifest holds the sidecar's bytes, which a CRLF checkout of the fixture would change.
    let attrs = std::str::from_utf8(ATTRS).unwrap().replace("\r\n", "\n");
    fs::write(source.join("bucket-one/folder/body.metadata"), attrs).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&body)
        .unwrap()
        .set_modified(
            std::time::UNIX_EPOCH + std::time::Duration::from_nanos(1_700_000_000_123_456_700),
        )
        .unwrap();
    let options = Options::new(
        source,
        directory.0.join("destination"),
        MetadataMode::Sidecar,
        SourcePlatform::Unix,
    );
    (directory, options, body)
}

/// The stat observation digest of one source body, as a scan computes it.
fn observation_digest(path: &str, fingerprint: &Fingerprint) -> String {
    let (send, _receive) = std::sync::mpsc::sync_channel(1);
    let mut reporter = Reporter {
        send: PageSender::Sync(send),
        page: Page::default(),
        summary: Summary::default(),
        manifest: digest_context(b"export"),
        observations: digest_context(b"stat"),
        body_manifest: digest_context(b"bodies"),
        baseline: None,
        purpose: ScanPurpose::Inspect,
    };
    reporter.observations(path, fingerprint);
    finish_digest(reporter.observations)
}

/// An import's journal binding holds its manifest digest, and every later scan must reproduce
/// both digests, so they keep the bytes earlier releases computed: a change strands every
/// unfinished import those releases started.
#[test]
fn inspection_digests_keep_the_bytes_earlier_imports_recorded() {
    let (_directory, options, body) = digest_fixture();
    let (summary, pages) = xattr_report(options);
    assert!(summary.eligible, "{pages:?}");
    assert_eq!(
        summary.manifest_sha256.as_deref(),
        Some("2d5bf7101e8f74b42389c8a1f54e28335605fa0bd69f63fb66653e65b767ea5f")
    );
    assert_eq!(
        summary.stat_observations_sha256,
        Some(observation_digest(
            "bucket-one/folder/body",
            &Fingerprint::of_path(&body).unwrap()
        ))
    );
    for (fingerprint, digest) in [
        (
            Fingerprint {
                size: 5,
                mtime_ns: 1_700_000_000_123_456_700,
                ctime_ns: Some(1_700_000_001_000_000_001),
                inode: Some(42),
                device: Some(7),
            },
            "e5906fba9f6b5625773c171f7aa7fe41bf444587b1ff340476e0e09e7c04f1f3",
        ),
        (
            Fingerprint {
                size: 5,
                mtime_ns: -1,
                ctime_ns: None,
                inode: None,
                device: None,
            },
            "0a34c68cce67b317bb833c31d9b7c73fa948df1a6eff0272bf5b35d37c3ef65f",
        ),
    ] {
        assert_eq!(
            observation_digest("bucket-one/folder/body", &fingerprint),
            digest,
            "{fingerprint:?}"
        );
    }
}

/// The dry-run report is how operators read an inspection: each record keeps its fields, their
/// order and their encoding.
#[test]
fn dry_run_records_keep_their_report_encoding() {
    let (_directory, mut options, body) = digest_fixture();
    options.verify_bodies = true;
    let (summary, pages) = xattr_report(options);
    assert!(summary.eligible && summary.bodies_verified, "{pages:?}");
    let records: Vec<&serde_json::Value> = pages
        .iter()
        .filter_map(|page| page["page"]["records"].as_array())
        .flatten()
        .collect();
    let [bucket, directory, object] = records.as_slice() else {
        panic!("expected three records: {records:?}")
    };
    let observed_stat = bucket["observed_stat"].as_str().unwrap();
    assert!(
        observed_stat.starts_with("report-only modified time: "),
        "{observed_stat}"
    );
    assert_eq!(
        serde_json::to_string(bucket).unwrap(),
        format!(
            r#"{{"kind":"bucket","path":"bucket-one","name":"bucket-one","attributes":{{"DefaultEventBasedHold":true,"ACL":null}},"created_timestamp":null,"enable_object_retention":false,"generated":["project_local-project","native_location","native_storage_class","metageneration_1","creation_time","no_iam_grants"],"observed_stat":{}}}"#,
            serde_json::to_string(observed_stat).unwrap()
        )
    );
    assert_eq!(
        serde_json::to_string(directory).unwrap(),
        r#"{"kind":"directory","path":"bucket-one/folder"}"#
    );
    assert_eq!(
        serde_json::to_string(object).unwrap(),
        format!(
            r#"{{"kind":"object","path":"bucket-one/folder/body","bucket":"bucket-one","name":"folder/body","attributes":{{"StorageClass":"STANDARD","ContentType":"text/plain","ContentEncoding":"","ContentDisposition":"inline","ContentLanguage":"en","CacheControl":"private","Crc32c":"mnG7TA==","Md5Hash":"XUFAKrxLKna5cZ2REBfFkg==","Etag":"upstream-custom-value","ACL":null,"Metadata":{{"source":"fixture"}},"Created":"1969-12-31T23:59:59.999999Z","Deleted":"0001-01-01T00:00:00Z","Updated":"2026-10-24T09:00:00.123456000+09:00","CustomTime":"0001-01-01T00:00:00Z","Generation":1792800000123456,"RetentionMode":"","RetentionRetainUntil":"0001-01-01T00:00:00Z"}},"created_micros":-1,"updated_micros":1792800000123456,"deleted_micros":null,"fingerprint":{},"verified_hashes":{{"md5":[93,65,64,42,188,75,42,118,185,113,157,145,16,23,197,146],"crc32c":2591144780}},"generated":["native_etag","metageneration_1"]}}"#,
            serde_json::to_string(&Fingerprint::of_path(&body).unwrap()).unwrap()
        )
    );
}

/// Writes `bytes` to `path` with a modification time long past, so a later write always moves it.
fn write_aged(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))
        .unwrap();
}

fn append(path: &std::path::Path) {
    use std::io::Write as _;
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(b" world")
        .unwrap();
}

fn changed_fields(result: Result<(), ImportFailure>) -> Vec<ChangedField> {
    match result {
        Err(ImportFailure::SourceChanged { fields }) => fields,
        other => panic!("expected a source change: {other:?}"),
    }
}

fn is_strictly_sorted(fields: &[ChangedField]) -> bool {
    fields.is_sorted_by(|before, after| before < after)
}

/// A source read reports every field that moved on its handle or under its name, in
/// `ChangedField` order, so a file replaced while its handle also drifted is never mistaken for
/// stat drift that a retry resolves.
#[test]
fn pinned_sources_report_every_field_that_moved_on_either_side() {
    use reader::PinnedSource;
    let directory = TestDir::new();
    let path = directory.0.join("source");
    let replacement = directory.0.join("replacement");
    let named = || Fingerprint::of_path(&path);

    write_aged(&path, b"hello");
    let source = PinnedSource::open(&path).unwrap();
    assert!(source.verify_unchanged(named).is_ok());

    // Replaced under its name with the same bytes and times: the name denotes another file.
    write_aged(&replacement, b"hello");
    fs::rename(&replacement, &path).unwrap();
    let fields = changed_fields(source.verify_unchanged(named));
    assert!(
        fields.contains(&ChangedField::Inode)
            && !fields.contains(&ChangedField::Size)
            && is_strictly_sorted(&fields),
        "{fields:?}"
    );

    // Appended in place: the handle moved, and the name still denotes the same file.
    write_aged(&path, b"hello");
    let source = PinnedSource::open(&path).unwrap();
    append(&path);
    let fields = changed_fields(source.verify_unchanged(named));
    assert!(
        fields.starts_with(&[ChangedField::Size, ChangedField::Mtime])
            && !fields.contains(&ChangedField::Inode)
            && is_strictly_sorted(&fields),
        "{fields:?}"
    );

    // Appended, then replaced: both sides moved, and their union needs the source repaired.
    write_aged(&path, b"hello");
    let source = PinnedSource::open(&path).unwrap();
    append(&path);
    write_aged(&replacement, b"hello world!");
    fs::rename(&replacement, &path).unwrap();
    let fields = changed_fields(source.verify_unchanged(named));
    assert!(
        fields.starts_with(&[ChangedField::Size, ChangedField::Mtime, ChangedField::Inode])
            && is_strictly_sorted(&fields),
        "{fields:?}"
    );
    assert!(!ImportFailure::SourceChanged { fields }.retryable_stat_drift());

    // Appended, then removed: a read needs its name, so the failure is the lookup's.
    #[cfg(unix)]
    {
        write_aged(&path, b"hello");
        let source = PinnedSource::open(&path).unwrap();
        append(&path);
        fs::remove_file(&path).unwrap();
        std::assert_matches!(
                source.verify_unchanged(named),
                Err(ImportFailure::Io(error)) if error.kind() == io::ErrorKind::NotFound,
            "a read needs its name"
        );
    }
}

/// Each refused command line names one option and its remedy. When a line has several mistakes,
/// the first one reported is fixed: operators fix them in that order.
#[test]
fn command_line_refusals_name_their_option_and_remedy() {
    const VALUE: &str = "Supply exactly one nonempty value; see import fake-gcs-server --help.";
    const UNKNOWN: &str =
        "Remove the unknown or duplicate option; see import fake-gcs-server --help.";
    const REQUIRED: &str = "Supply the required option; see import fake-gcs-server --help.";
    const SNAPSHOT_NAMES: &str =
        "Snapshot names and attributes are explicit; remove --metadata-mode and --source-platform.";
    let filesystem = [
        "--metadata-mode",
        "sidecar",
        "--source-platform",
        "unix",
        "--source",
        "source",
        "--data-dir",
        "data",
    ];
    let with = |extra: &[&'static str]| [&filesystem[..], extra].concat();
    for (args, key, fix) in [
        (vec!["--source"], "--source", VALUE),
        (vec!["--source", ""], "--source", VALUE),
        (vec!["--source", "--dry-run"], "--source", VALUE),
        (vec!["--max-depth"], "--max-depth", VALUE),
        (
            vec!["--source", "a", "--source", "b"],
            "--source",
            "Remove the duplicate option.",
        ),
        (vec!["--unknown"], "--unknown", UNKNOWN),
        (vec!["--dry-run", "--dry-run"], "--dry-run", UNKNOWN),
        (
            vec!["--dry-run", "--verify-bodies", "--verify-bodies"],
            "--verify-bodies",
            UNKNOWN,
        ),
        (
            vec!["--verify-bodies", "--source-format", "bad"],
            "--verify-bodies",
            "Use --dry-run --verify-bodies for inspection; actual import always verifies bodies.",
        ),
        (
            vec!["--source-format", "bad", "--metadata-mode", "bad"],
            "--source-format",
            "Use filesystem or snapshot.",
        ),
        (
            vec![
                "--source-format",
                "snapshot",
                "--metadata-mode",
                "sidecar",
                "--max-depth",
                "1",
            ],
            "--source-format",
            SNAPSHOT_NAMES,
        ),
        (
            vec!["--source-format", "snapshot", "--source-platform", "unix"],
            "--source-format",
            SNAPSHOT_NAMES,
        ),
        (
            vec!["--source-format", "snapshot", "--max-depth", "1"],
            "--max-depth",
            "A snapshot holds rows, not directories; remove --max-depth.",
        ),
        (
            vec!["--source-format", "snapshot", "--data-dir", "data"],
            "--source",
            REQUIRED,
        ),
        (
            vec!["--source-platform", "bad"],
            "--metadata-mode",
            REQUIRED,
        ),
        (
            vec!["--metadata-mode", "bad", "--source-platform", "bad"],
            "--metadata-mode",
            "Use xattr or sidecar, matching the entire source root.",
        ),
        (
            vec!["--metadata-mode", "xattr", "--source", "source"],
            "--source-platform",
            REQUIRED,
        ),
        (
            vec!["--metadata-mode", "xattr", "--source-platform", "bad"],
            "--source-platform",
            "Use unix or windows, matching the source path platform.",
        ),
        (
            vec![
                "--metadata-mode",
                "xattr",
                "--source-platform",
                "windows",
                "--data-dir",
                "data",
            ],
            "--source",
            REQUIRED,
        ),
        (
            vec![
                "--metadata-mode",
                "xattr",
                "--source-platform",
                "windows",
                "--source",
                "source",
                "--max-depth",
                "bad",
            ],
            "--data-dir",
            REQUIRED,
        ),
        (
            with(&["--max-depth", "1025", "--max-object-bytes", "5497558138881"]),
            "--max-object-bytes",
            "Use an integer in 0..=5497558138880; see import fake-gcs-server --help.",
        ),
        (
            with(&["--max-object-bytes", "-1"]),
            "--max-object-bytes",
            "Use an integer in 0..=5497558138880; see import fake-gcs-server --help.",
        ),
        (
            with(&[
                "--max-depth",
                "1025",
                "--max-entries",
                "18446744073709551616",
            ]),
            "--max-entries",
            "Use an integer in 0..=18446744073709551615; see import fake-gcs-server --help.",
        ),
        (
            with(&["--max-depth", "1025"]),
            "--max-depth",
            "Use an integer in 0..=1024; see import fake-gcs-server --help.",
        ),
    ] {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let Err(ImportError::Diagnostics(refused)) = cli(&args) else {
            panic!("{args:?} was not refused by its options")
        };
        let [error] = refused.errors.as_slice() else {
            panic!("{args:?}: {refused:?}")
        };
        assert_eq!(
            (error.source, error.key.as_str(), error.fix.as_str()),
            (DiagnosticSource::Cli, key, fix),
            "{args:?}"
        );
        assert!(refused.warnings.is_empty() && refused.info.is_empty());
    }
}

/// Report keys are `{path}:{field}`, and operators and tests match them, so each named field and
/// each other failure keeps the key it had when the names were strings.
#[test]
fn source_failures_keep_their_report_keys() {
    for (field, key) in [
        (CorruptField::Name, "name"),
        (CorruptField::Metadata, "metadata"),
        (CorruptField::UserMetadata, "user.metadata"),
        (CorruptField::Entry, "entry"),
        (CorruptField::Limits, "limits"),
        (CorruptField::Size, "Size"),
        (CorruptField::BucketAttrs, "BucketAttrs"),
        (CorruptField::ObjectAttrs, "ObjectAttrs"),
        (CorruptField::Snapshot, "snapshot"),
    ] {
        let failure = ImportFailure::corrupt(field, "reason", "fix");
        assert_eq!((failure.field(), failure.fix()), (key, "fix"));
    }
    for (failure, key) in [
        (
            ImportFailure::SourceChanged {
                fields: vec![ChangedField::Size],
            },
            "fingerprint",
        ),
        (
            ImportFailure::from(io::Error::from(io::ErrorKind::Other)),
            "io",
        ),
        (ImportFailure::ReportClosed, "report"),
    ] {
        assert_eq!(failure.field(), key);
    }
}
