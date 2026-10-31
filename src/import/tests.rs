#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::fsutil::{SourceMetadataBuffer, SourceMetadataError, test_support::SourceDir};
use std::fs::File;

fn xattr_setup() -> (SourceDir, Options) {
    let directory = SourceDir::new();
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
                    entity: "allAuthenticatedUsers".into(),
                    role: "WRITER".into()
                }]
            );
            assert!(projection.objects[0].is_empty());
            assert_eq!(
                projection.objects[1],
                vec![crate::acl::AclEntry {
                    entity: "allUsers".into(),
                    role: "READER".into()
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
    struct StopAfterObject(usize);
    impl std::io::Write for StopAfterObject {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.contains(&b'\n') {
                self.0 += 1;
                if self.0 == 2 {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
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
    assert!(execute(options.clone(), StopAfterObject(0), Vec::new()).is_err());
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
    let directory = SourceDir::new();
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
    let directory = SourceDir::new();
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
