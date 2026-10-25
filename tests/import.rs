#[allow(dead_code)]
mod common;

use common::TestDir;
use gcs_mimic::{
    import::{
        self, BodyStream, ChangedField, Fingerprint, ImportFailure, MetadataMode, Options,
        SourcePlatform,
    },
    source_tree::{self, Walker},
};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const ATTRS: &[u8] = include_bytes!("fixtures/import/object-attrs.json");

fn setup() -> (TestDir, Options) {
    let dir = TestDir::new();
    let root = dir.0.join("source");
    fs::create_dir_all(root.join("bucket-one")).unwrap();
    let options = Options::new(
        root,
        dir.0.join("destination"),
        MetadataMode::Sidecar,
        SourcePlatform::Unix,
    );
    (dir, options)
}

fn object(root: &Path, name: &str, attrs: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"hello").unwrap();
    fs::write(format!("{}.metadata", path.display()), attrs).unwrap();
}

fn inspect(options: Options) -> (import::Summary, Vec<Value>, String) {
    let (mut output, mut diagnostics) = (Vec::new(), Vec::new());
    let summary = import::inspect(options, &mut output, &mut diagnostics).unwrap();
    let pages = std::str::from_utf8(&output)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (summary, pages, String::from_utf8(diagnostics).unwrap())
}

fn assert_finding(pages: &[Value], key: &str, reason: &str) {
    let matches: Vec<_> = pages
        .iter()
        .filter_map(|p| p["page"]["findings"].as_array())
        .flatten()
        .filter(|f| {
            f["diagnostic"]["key"] == key
                && f["reason"].as_str().is_some_and(|r| r.contains(reason))
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected {key}: {reason}; report {pages:?}"
    );
}

fn export_digest(summary: &import::Summary) -> &str {
    assert!(summary.eligible);
    summary.manifest_sha256.as_deref().unwrap()
}

#[test]
fn sidecar_inspection_is_read_only_and_distinguishes_body_verification() {
    let (_dir, mut options) = setup();
    fs::create_dir(options.source.join("empty-bucket")).unwrap();
    fs::write(
        options.source.join("empty-bucket.bucketMetadata"),
        b"{\"DefaultEventBasedHold\":true}",
    )
    .unwrap();
    object(&options.source, "bucket-one/folder/hello%2F.txt", ATTRS);
    let path = options.source.join("bucket-one/folder/hello%2F.txt");
    let before = Fingerprint::of(&fs::metadata(&path).unwrap()).unwrap();
    let attrs_path = options
        .source
        .join("bucket-one/folder/hello%2F.txt.metadata");
    let attrs_before = Fingerprint::of(&fs::metadata(&attrs_path).unwrap()).unwrap();
    let permissions = fs::metadata(&path).unwrap().permissions();
    let (summary, pages, _) = inspect(options.clone());
    assert!(
        summary.complete && summary.eligible && !summary.bodies_verified,
        "{pages:?}"
    );
    assert_eq!(
        (summary.buckets, summary.objects, summary.bytes_hashed),
        (2, 1, 0)
    );
    assert_eq!(pages[0]["original_names_verified"], false);
    let metadata_manifest = summary.manifest_sha256;
    let records: Vec<_> = pages
        .iter()
        .filter_map(|p| p["page"]["records"].as_array())
        .flatten()
        .collect();
    let record = records.iter().find(|r| r["kind"] == "object").unwrap();
    assert_eq!(record["name"], "folder/hello%2F.txt");
    assert_eq!(record["created_micros"], -1);
    assert_eq!(record["attributes"]["Etag"], "upstream-custom-value");
    options.verify_bodies = true;
    let (summary, _, _) = inspect(options.clone());
    assert!(summary.bodies_verified);
    assert_eq!(metadata_manifest, summary.manifest_sha256);
    assert_eq!(summary.bytes_hashed, 5);
    assert!(summary.body_digests_sha256.is_some());
    assert_eq!(
        before,
        Fingerprint::of(&fs::metadata(&path).unwrap()).unwrap()
    );
    assert_eq!(
        attrs_before,
        Fingerprint::of(&fs::metadata(&attrs_path).unwrap()).unwrap()
    );
    assert_eq!(fs::read(path).unwrap(), b"hello");
    assert_eq!(fs::read(attrs_path).unwrap(), ATTRS);
    assert_eq!(
        permissions,
        fs::metadata(options.source.join("bucket-one/folder/hello%2F.txt"))
            .unwrap()
            .permissions()
    );
    assert!(!options.destination.exists());
}

#[test]
fn corrupt_records_are_all_reported_with_relative_fields_original_values_and_remedies() {
    let (_dir, mut options) = setup();
    let mut attrs: Value = serde_json::from_slice(ATTRS).unwrap();
    attrs["Created"] = json!("2026-10-24T00:00:00.123456001Z");
    attrs["ACL"] = json!([{"Entity":"allUsers","Role":"READER"}]);
    attrs["CustomTime"] = json!("2026-01-01T00:00:00Z");
    attrs["RetentionMode"] = json!("Locked");
    attrs["Md5Hash"] = json!("bad");
    attrs["Updated"] = json!("not-a-time");
    object(
        &options.source,
        "bucket-one/bad",
        &serde_json::to_vec(&attrs).unwrap(),
    );
    object(&options.source, "bucket-one/mismatch", ATTRS);
    fs::write(options.source.join("bucket-one/mismatch"), b"world").unwrap();
    fs::write(options.source.join("bucket-one/orphan.metadata"), ATTRS).unwrap();
    fs::write(options.source.join("orphan-bucket.bucketMetadata"), b"{}").unwrap();
    fs::write(options.source.join("bucket-one/missing"), b"hello").unwrap();
    object(
        &options.source,
        "bucket-one/unknown",
        b"{\"Name\":\"injected\"}",
    );
    object(
        &options.source,
        "bucket-one/duplicate",
        b"{\"Created\":\"a\",\"Created\":\"b\"}",
    );
    object(&options.source, "bucket-one/broken", b"{");
    object(
        &options.source,
        "bucket-one/wrong-type",
        b"{\"Generation\":\"1\"}",
    );
    object(
        &options.source,
        "bucket-one/duplicate-map",
        b"{\"Metadata\":{\"x\":\"1\",\"x\":\"2\"}}",
    );
    options.verify_bodies = true;
    let (summary, pages, diagnostics) = inspect(options.clone());
    assert!(summary.complete && !summary.eligible && !summary.bodies_verified);
    assert_eq!(summary.blocking_findings, 16, "{pages:?}");
    for (key, reason) in [
        ("bucket-one/bad:Created", "nonzero sub-microsecond"),
        ("bucket-one/bad:Updated", "invalid source timestamp"),
        ("bucket-one/bad:ACL", "persisted ACL semantics"),
        ("bucket-one/bad:CustomTime", "active source semantics"),
        (
            "bucket-one/bad:RetentionMode",
            "retention cannot be enforced",
        ),
        ("bucket-one/bad:Md5Hash", "invalid canonical base64 MD5"),
        ("bucket-one/mismatch:Md5Hash", "body checksum mismatch"),
        ("bucket-one/mismatch:Crc32c", "body checksum mismatch"),
        ("bucket-one/unknown:ObjectAttrs", "unknown field"),
        ("bucket-one/duplicate:ObjectAttrs", "duplicate field"),
        (
            "bucket-one/broken:ObjectAttrs",
            "invalid object metadata JSON",
        ),
        ("bucket-one/wrong-type:ObjectAttrs", "invalid type"),
        (
            "bucket-one/duplicate-map:ObjectAttrs",
            "duplicate metadata key",
        ),
        (
            "orphan-bucket.bucketMetadata:BucketAttrs",
            "orphan bucket metadata",
        ),
    ] {
        assert_finding(&pages, key, reason);
    }
    assert_finding(
        &pages,
        "bucket-one/missing:metadata",
        "missing object metadata sidecar",
    );
    assert_finding(
        &pages,
        "bucket-one/orphan.metadata:metadata",
        "body is missing",
    );
    let text = serde_json::to_string(&pages).unwrap();
    for expected in [
        "Created",
        "Updated",
        "ACL",
        "CustomTime",
        "RetentionMode",
        "checksum mismatch",
        "orphan",
        "duplicate field",
        "unknown field",
        "123456001",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert!(!text.contains(options.source.to_str().unwrap()));
    assert!(
        diagnostics
            .lines()
            .all(|line| serde_json::from_str::<Value>(line).unwrap()["event"] == "diagnostics")
    );
}

#[test]
fn normalized_source_names_are_reported_without_inventing_original_names() {
    let (_dir, options) = setup();
    let writes: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/import/normalized-writes.json")).unwrap();
    for write in &writes {
        object(&options.source, write["disk"].as_str().unwrap(), ATTRS);
    }
    let (summary, pages, _) = inspect(options);
    assert!(summary.eligible);
    assert_eq!(summary.objects, 3);
    assert_eq!(pages[0]["names_recovered_from_normalized_disk_paths"], true);
    assert_eq!(pages[0]["original_names_verified"], false);
    let text = serde_json::to_string(&pages).unwrap();
    assert!(!text.contains("a//b") && !text.contains("a/../b"));
    assert!(text.contains("other/y"));
}

#[test]
fn escaped_bucket_aliases_invalid_escapes_suffix_collisions_and_platform_rules_block() {
    let (_dir, options) = setup();
    fs::create_dir(options.source.join("%62ucket-one")).unwrap();
    fs::create_dir(options.source.join("bad%2")).unwrap();
    object(&options.source, "bucket-one/collision", ATTRS);
    fs::write(
        options
            .source
            .join("bucket-one/collision.metadata.metadata"),
        ATTRS,
    )
    .unwrap();
    let (summary, pages, _) = inspect(options.clone());
    assert!(!summary.eligible);
    let text = serde_json::to_string(&pages).unwrap();
    assert!(
        text.contains("duplicate decoded bucket")
            && text.contains("percent escape")
            && text.contains("suffix collision")
    );
    let mut invalid = options.clone();
    invalid.source_platform = SourcePlatform::Windows;
    invalid.metadata_mode = MetadataMode::Xattr;
    let error = import::inspect(invalid, Vec::new(), Vec::new()).unwrap_err();
    let diagnostics = error
        .downcast_ref::<gcs_mimic::diagnostics::Diagnostics>()
        .unwrap();
    assert_eq!(diagnostics.errors.len(), 1);
    assert_eq!(diagnostics.errors[0].key, "--metadata-mode");
    assert!(
        diagnostics.errors[0]
            .fix
            .contains("Windows source snapshots use sidecars")
    );
    #[cfg(unix)]
    {
        let (_dir, mut windows) = setup();
        windows.source_platform = SourcePlatform::Windows;
        object(&windows.source, "bucket-one/a\\b", ATTRS);
        let (summary, pages, _) = inspect(windows);
        assert!(!summary.eligible);
        assert_finding(
            &pages,
            "bucket-one/a\\b:name",
            "Windows-source backslash mapping is ambiguous",
        );
    }
}

#[test]
fn metadata_and_body_limits_empty_files_os_detritus_and_page_counts_are_explicit() {
    let (_dir, mut options) = setup();
    for index in 0..270 {
        object(
            &options.source,
            &format!("bucket-one/item-{index:03}"),
            ATTRS,
        );
    }
    fs::write(options.source.join(".DS_Store"), b"detritus").unwrap();
    fs::write(options.source.join("bucket-one/._detritus"), b"detritus").unwrap();
    object(&options.source, "bucket-one/.DS_Store", ATTRS);
    let (summary, pages, _) = inspect(options.clone());
    assert!(summary.eligible);
    assert_eq!((summary.skipped_os_metadata, summary.objects), (2, 271));
    assert!(pages.iter().filter(|p| p["event"] == "import_page").count() >= 2);
    assert!(
        pages
            .iter()
            .filter(|p| p["event"] == "import_page")
            .all(|p| p["page"]["records"].as_array().unwrap().len()
                + p["page"]["findings"].as_array().unwrap().len()
                <= 256)
    );
    options.max_entries = 2;
    let (summary, pages, _) = inspect(options);
    assert!(!summary.complete && !summary.eligible);
    assert_finding(&pages, ".:limits", "source entry limit exceeded");
    let (_dir, mut options) = setup();
    let mut exact = ATTRS.to_vec();
    exact.resize(65_536, b' ');
    object(&options.source, "bucket-one/exact", &exact);
    options.max_object_bytes = 5;
    assert!(inspect(options.clone()).0.eligible);
    exact.push(b' ');
    object(&options.source, "bucket-one/oversize", &exact);
    let (summary, pages, _) = inspect(options.clone());
    assert!(!summary.eligible);
    assert_finding(
        &pages,
        "bucket-one/oversize:metadata",
        "metadata exceeds 64 KiB",
    );
    let (_dir, mut body_options) = setup();
    object(&body_options.source, "bucket-one/body", ATTRS);
    body_options.max_object_bytes = 5;
    assert!(inspect(body_options.clone()).0.eligible);
    body_options.max_object_bytes = 4;
    let (summary, pages, _) = inspect(body_options);
    assert_eq!(summary.blocking_findings, 1);
    assert_finding(
        &pages,
        "bucket-one/body:Size",
        "body exceeds max_object_bytes",
    );
    let (_dir, mut options) = setup();
    let mut attrs: Value = serde_json::from_slice(ATTRS).unwrap();
    attrs["Md5Hash"] = json!("1B2M2Y8AsgTpgAmY7PhCfg==");
    attrs["Crc32c"] = json!("AAAAAA==");
    object(
        &options.source,
        "bucket-one/empty",
        &serde_json::to_vec(&attrs).unwrap(),
    );
    fs::write(options.source.join("bucket-one/empty"), b"").unwrap();
    options.max_object_bytes = 0;
    options.verify_bodies = true;
    assert!(inspect(options).0.bodies_verified);
}

#[test]
fn body_stream_bounds_chunks_and_detects_same_size_replacement_and_truncation() {
    let dir = TestDir::new();
    let path = dir.0.join("body");
    fs::write(&path, vec![42; 131_073]).unwrap();
    let mut body = BodyStream::open(&path, 131_073).unwrap();
    let mut buffer = [0; 65_536];
    assert_eq!(body.next_chunk(&mut buffer).unwrap().len(), 65_536);
    assert_eq!(body.next_chunk(&mut buffer).unwrap().len(), 65_536);
    assert_eq!(body.next_chunk(&mut buffer).unwrap().len(), 1);
    assert!(body.next_chunk(&mut buffer).unwrap().is_empty());
    body.check_unchanged().unwrap();
    fs::write(dir.0.join("replacement"), vec![42; 131_073]).unwrap();
    fs::rename(dir.0.join("replacement"), &path).unwrap();
    let Err(ImportFailure::SourceChanged { fields }) = body.check_unchanged() else {
        panic!("replacement must be detected")
    };
    #[cfg(unix)]
    assert!(fields.contains(&ChangedField::Inode));
    #[cfg(not(unix))]
    assert!(!fields.is_empty());
    let mut body = BodyStream::open(&path, 131_073).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(2)
        .unwrap();
    assert_eq!(body.next_chunk(&mut buffer).unwrap().len(), 2);
    assert!(matches!(
        body.check_unchanged(),
        Err(ImportFailure::SourceChanged { .. })
    ));
    assert!(
        ImportFailure::SourceChanged {
            fields: vec![ChangedField::Ctime, ChangedField::Inode]
        }
        .retryable_stat_drift()
    );
    assert!(
        !ImportFailure::SourceChanged {
            fields: vec![ChangedField::Metadata]
        }
        .retryable_stat_drift()
    );
}

#[test]
fn overlap_and_walk_fail_closed_without_creating_destination() {
    let (_dir, options) = setup();
    assert!(source_tree::paths_overlap(&options.source, &options.source.join("new")).unwrap());
    assert!(!source_tree::paths_overlap(&options.source, &options.destination).unwrap());
    let mut overlap = options.clone();
    overlap.destination = options.source.join("new");
    assert!(import::inspect(overlap, Vec::new(), Vec::new()).is_err());
    fs::create_dir_all(options.source.join("bucket-one/a/b/c")).unwrap();
    assert!(
        Walker::new(&options.source, 20, 1)
            .unwrap()
            .any(|entry| entry.is_err())
    );
    assert!(source_tree::is_os_metadata("._file") && source_tree::is_editor_temporary(".file.swp"));
    assert!(source_tree::is_ignored_regular_file("4913"));
    assert!(!source_tree::is_os_metadata("normal"));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&options.source, options.source.join("bucket-one/link"))
            .unwrap();
        let (summary, pages, _) = inspect(options.clone());
        assert!(!summary.eligible);
        assert_finding(
            &pages,
            "bucket-one/link:entry",
            "symlinks and special source entries are unsupported",
        );
        std::os::unix::fs::symlink(&options.source, &options.destination).unwrap();
        assert!(import::inspect(options, Vec::new(), Vec::new()).is_err());
    }
}

#[test]
fn cli_never_starts_serve_and_requires_explicit_read_only_mode() {
    let (_dir, options) = setup();
    object(&options.source, "bucket-one/body", ATTRS);
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
            .args(["import", "fake-gcs-server", "--source"])
            .arg(&options.source)
            .arg("--data-dir")
            .arg(&options.destination)
            .args(["--metadata-mode", "sidecar", "--source-platform", "unix"])
            .args(extra)
            .output()
            .unwrap()
    };
    let result = run(&["--dry-run", "--verify-bodies"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("\"bodies_verified\":true"));
    assert!(!text.contains("\"event\":\"ready\""));
    assert!(!options.destination.exists());
    assert!(!run(&["--verify-bodies"]).status.success());
    assert!(!run(&["--dry-run", "--dry-run"]).status.success());
    let help = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(["import", "fake-gcs-server", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
}

#[test]
fn broken_report_output_joins_the_bounded_worker_without_destination_writes() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed consumer",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (_dir, options) = setup();
    for index in 0..260 {
        object(&options.source, &format!("bucket-one/{index}"), ATTRS);
    }
    struct HeaderThenBroken {
        lines: usize,
    }
    impl std::io::Write for HeaderThenBroken {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.lines > 0 {
                return std::io::Write::write(&mut Broken, bytes);
            }
            self.lines += bytes.iter().filter(|b| **b == b'\n').count();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(import::inspect(options.clone(), HeaderThenBroken { lines: 0 }, Vec::new()).is_err());
    assert!(!options.destination.exists());
    assert_eq!(
        fs::read(options.source.join("bucket-one/0")).unwrap(),
        b"hello"
    );
}

#[test]
fn native_storage_class_and_fixed_metadata_validation_matches_import_eligibility() {
    for (field, value, reason) in [
        ("StorageClass", json!(""), "invalid object storageClass"),
        (
            "StorageClass",
            json!("standard"),
            "invalid object storageClass",
        ),
        (
            "StorageClass",
            json!("UNKNOWN"),
            "invalid object storageClass",
        ),
        (
            "StorageClass",
            json!("REGIONAL"),
            "storageClass requires a region or zone",
        ),
        (
            "ContentDisposition",
            json!("filename=日本語.txt"),
            "non-ASCII fixed metadata",
        ),
        (
            "CacheControl",
            json!("private\tmax-age=0"),
            "Invalid cacheControl header value",
        ),
        (
            "ContentLanguage",
            json!("x".repeat(101)),
            "contentLanguage exceeds 100 characters",
        ),
    ] {
        let (_dir, options) = setup();
        let mut attrs: Value = serde_json::from_slice(ATTRS).unwrap();
        attrs[field] = value;
        object(
            &options.source,
            "bucket-one/body",
            &serde_json::to_vec(&attrs).unwrap(),
        );
        let (summary, pages, _) = inspect(options);
        assert_eq!(
            (
                summary.objects,
                summary.eligible_objects,
                summary.blocking_findings
            ),
            (1, 0, 1)
        );
        assert_finding(&pages, &format!("bucket-one/body:{field}"), reason);
        if reason == "invalid object storageClass" {
            let finding = pages
                .iter()
                .filter_map(|p| p["page"]["findings"].as_array())
                .flatten()
                .find(|f| f["diagnostic"]["key"] == "bucket-one/body:StorageClass")
                .unwrap();
            assert!(
                finding["diagnostic"]["fix"]
                    .as_str()
                    .unwrap()
                    .contains("exact supported object storageClass")
            );
            assert!(
                !finding["diagnostic"]["fix"]
                    .as_str()
                    .unwrap()
                    .contains("REGIONAL")
            );
        }
    }
    let (_dir, options) = setup();
    fs::create_dir(options.source.join("b1")).unwrap();
    let (_, pages, _) = inspect(options);
    assert_finding(&pages, "b1:name", "invalid decoded bucket name");
}

#[test]
fn discovered_counts_and_eligible_records_agree_for_buckets_and_objects() {
    let (_dir, options) = setup();
    fs::create_dir(options.source.join("blocked-bucket")).unwrap();
    fs::write(
        options.source.join("blocked-bucket.bucketMetadata"),
        b"{\"VersioningEnabled\":true}",
    )
    .unwrap();
    object(&options.source, "bucket-one/good", ATTRS);
    object(&options.source, "bucket-one/bad", b"{");
    object(&options.source, "blocked-bucket/body", ATTRS);
    let (summary, pages, _) = inspect(options);
    assert_eq!(
        (
            summary.buckets,
            summary.eligible_buckets,
            summary.objects,
            summary.eligible_objects
        ),
        (2, 1, 3, 1)
    );
    assert!(
        !pages
            .iter()
            .filter_map(|p| p["page"]["findings"].as_array())
            .flatten()
            .any(|f| f["diagnostic"]["key"] == "blocked-bucket/body:bucket")
    );
    let records: Vec<_> = pages
        .iter()
        .filter_map(|p| p["page"]["records"].as_array())
        .flatten()
        .collect();
    assert_eq!(records.iter().filter(|r| r["kind"] == "bucket").count(), 1);
    assert_eq!(records.iter().filter(|r| r["kind"] == "object").count(), 1);
}

#[test]
fn export_fingerprint_tracks_layout_and_skips_but_separates_stat_drift() {
    let (_dir, options) = setup();
    object(&options.source, "bucket-one/body", ATTRS);
    let first = inspect(options.clone()).0;
    fs::create_dir(options.source.join("bucket-one/empty")).unwrap();
    let directory = inspect(options.clone()).0;
    assert_ne!(export_digest(&first), export_digest(&directory));
    fs::write(options.source.join("bucket-one/._detritus"), b"detritus").unwrap();
    let skipped = inspect(options.clone()).0;
    assert_ne!(export_digest(&directory), export_digest(&skipped));
    let body = options.source.join("bucket-one/body");
    let modified = fs::metadata(&body).unwrap().modified().unwrap();
    let replacement = options.source.join("bucket-one/replacement");
    fs::write(&replacement, b"hello").unwrap();
    fs::File::open(&replacement)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    fs::rename(replacement, body).unwrap();
    let replaced = inspect(options.clone()).0;
    assert_eq!(export_digest(&skipped), export_digest(&replaced));
    #[cfg(unix)]
    assert_ne!(
        skipped.stat_observations_sha256,
        replaced.stat_observations_sha256
    );
    assert_eq!(replaced.fingerprint_format, 1);
    let hash = replaced.manifest_sha256.as_ref().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    let body = options.source.join("bucket-one/body");
    fs::File::open(&body)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified + std::time::Duration::from_secs(1)))
        .unwrap();
    let mtime = inspect(options.clone()).0;
    assert_ne!(export_digest(&replaced), export_digest(&mtime));
    let stable_mtime = fs::metadata(&body).unwrap().modified().unwrap();
    fs::write(&body, b"hello world").unwrap();
    fs::File::open(&body)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(stable_mtime))
        .unwrap();
    let size = inspect(options.clone()).0;
    assert_ne!(export_digest(&mtime), export_digest(&size));
    let mut metadata = ATTRS.to_vec();
    metadata.push(b' ');
    fs::write(options.source.join("bucket-one/body.metadata"), metadata).unwrap();
    let attrs = inspect(options.clone()).0;
    assert_ne!(export_digest(&size), export_digest(&attrs));
    let mut platform = options.clone();
    platform.source_platform = SourcePlatform::Windows;
    assert_ne!(export_digest(&attrs), export_digest(&inspect(platform).0));
    let mut limit = options.clone();
    limit.max_object_bytes += 1;
    assert_ne!(export_digest(&attrs), export_digest(&inspect(limit).0));
    let mut incomplete = options.clone();
    incomplete.max_entries = 1;
    let incomplete = inspect(incomplete).0;
    assert!(
        !incomplete.complete
            && incomplete.manifest_sha256.is_none()
            && incomplete.stat_observations_sha256.is_none()
    );
    let mut shallow = options.clone();
    shallow.max_depth = 0;
    let (_, shallow_pages, _) = inspect(shallow);
    assert_finding(&shallow_pages, ".:limits", "source depth limit exceeded");
    let fix = shallow_pages
        .iter()
        .filter_map(|p| p["page"]["findings"].as_array())
        .flatten()
        .find(|f| f["diagnostic"]["key"] == ".:limits")
        .unwrap();
    assert!(
        fix["diagnostic"]["fix"]
            .as_str()
            .unwrap()
            .contains("within 0 levels")
    );
    fs::write(options.source.join("bucket-one/body.metadata"), b"{").unwrap();
    let blocked = inspect(options).0;
    assert!(
        blocked.complete
            && !blocked.eligible
            && blocked.manifest_sha256.is_none()
            && blocked.stat_observations_sha256.is_none()
    );
}

#[test]
fn duplicate_bucket_aliases_never_emit_eligible_child_records() {
    let (_dir, options) = setup();
    object(&options.source, "abc/x", ATTRS);
    object(&options.source, "a%62c/x", ATTRS);
    object(&options.source, "a%62c/bad", b"{");
    fs::create_dir(options.source.join("abc/directory")).unwrap();
    fs::create_dir(options.source.join("a%62c/directory")).unwrap();
    let (summary, pages, _) = inspect(options);
    assert_eq!(summary.directories, 2);
    assert_eq!(
        (
            summary.buckets,
            summary.eligible_buckets,
            summary.objects,
            summary.eligible_objects
        ),
        (3, 1, 3, 0)
    );
    assert_finding(&pages, "abc:name", "duplicate decoded bucket name");
    assert_finding(&pages, "a%62c:name", "duplicate decoded bucket name");
    assert_finding(
        &pages,
        "a%62c/bad:ObjectAttrs",
        "invalid object metadata JSON",
    );
    assert!(
        !pages
            .iter()
            .filter_map(|p| p["page"]["records"].as_array())
            .flatten()
            .any(|r| r["kind"] == "object" || r["kind"] == "directory")
    );

    let (_dir, options) = setup();
    fs::write(options.source.join("abc"), b"not a bucket").unwrap();
    object(&options.source, "a%62c/x", ATTRS);
    fs::create_dir(options.source.join("a%62c/directory")).unwrap();
    let (summary, pages, _) = inspect(options);
    assert_eq!(
        (
            summary.objects,
            summary.directories,
            summary.eligible_objects
        ),
        (1, 1, 0)
    );
    assert_finding(&pages, "a%62c:name", "duplicate decoded bucket name");
    assert!(
        !pages
            .iter()
            .filter_map(|p| p["page"]["records"].as_array())
            .flatten()
            .any(|r| r["kind"] == "object" || r["kind"] == "directory")
    );
}

#[test]
fn invalid_parent_names_and_attributes_keep_child_diagnostics() {
    let (_dir, options) = setup();
    for parent in ["a%zz", "badattrs", "x"] {
        object(&options.source, &format!("{parent}/bad"), b"{");
        fs::write(options.source.join(format!("{parent}/nometa")), b"hello").unwrap();
        fs::create_dir(options.source.join(format!("{parent}/directory"))).unwrap();
    }
    fs::write(options.source.join("badattrs.bucketMetadata"), b"{").unwrap();
    let (summary, pages, _) = inspect(options);
    assert_eq!(
        (
            summary.buckets,
            summary.objects,
            summary.directories,
            summary.eligible_buckets,
            summary.eligible_objects
        ),
        (4, 6, 3, 1, 0)
    );
    for parent in ["a%zz", "badattrs", "x"] {
        assert_finding(
            &pages,
            &format!("{parent}/bad:ObjectAttrs"),
            "invalid object metadata JSON",
        );
        assert_finding(
            &pages,
            &format!("{parent}/nometa:metadata"),
            "missing object metadata sidecar",
        );
        assert_finding(
            &pages,
            &format!("{parent}/directory:name"),
            "directory-only candidate",
        );
    }
    assert_finding(&pages, "a%zz:name", "malformed bucket percent escape");
    assert_finding(
        &pages,
        "badattrs:BucketAttrs",
        "invalid bucket metadata JSON",
    );
    assert_finding(&pages, "x:name", "invalid decoded bucket name");
    let records: Vec<_> = pages
        .iter()
        .filter_map(|p| p["page"]["records"].as_array())
        .flatten()
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["kind"], "bucket");
    assert_eq!(records[0]["name"], "bucket-one");
    assert!(
        !pages
            .iter()
            .filter_map(|p| p["page"]["findings"].as_array())
            .flatten()
            .any(|f| f["diagnostic"]["key"]
                .as_str()
                .unwrap()
                .ends_with(":bucket"))
    );
}

#[test]
fn blocked_bucket_reports_child_corruption_without_emitting_records() {
    let (_dir, mut options) = setup();
    options.verify_bodies = true;
    fs::write(
        options.source.join("bucket-one.bucketMetadata"),
        b"{\"VersioningEnabled\":true}",
    )
    .unwrap();
    object(&options.source, "bucket-one/bad", b"{");
    fs::write(options.source.join("bucket-one/nometa"), b"hello").unwrap();
    object(&options.source, "bucket-one/good", ATTRS);
    fs::create_dir(options.source.join("bucket-one/directory")).unwrap();
    let (summary, pages, _) = inspect(options.clone());
    assert_eq!(
        (
            summary.buckets,
            summary.objects,
            summary.directories,
            summary.eligible_buckets,
            summary.eligible_objects
        ),
        (1, 3, 1, 0, 0)
    );
    assert_eq!(summary.blocking_findings, 3);
    assert_finding(
        &pages,
        "bucket-one:VersioningEnabled",
        "unsupported persisted bucket semantics",
    );
    assert_finding(
        &pages,
        "bucket-one/bad:ObjectAttrs",
        "invalid object metadata JSON",
    );
    assert_finding(
        &pages,
        "bucket-one/nometa:metadata",
        "missing object metadata sidecar",
    );
    assert!(
        !pages
            .iter()
            .filter_map(|p| p["page"]["records"].as_array())
            .flatten()
            .any(|_| true)
    );
    assert!(
        !pages
            .iter()
            .filter_map(|p| p["page"]["findings"].as_array())
            .flatten()
            .any(|f| f["diagnostic"]["key"]
                .as_str()
                .unwrap()
                .ends_with(":bucket"))
    );
    fs::write(options.source.join("bucket-one/good"), b"world").unwrap();
    let (_, pages, _) = inspect(options);
    assert_finding(&pages, "bucket-one/good:Md5Hash", "body checksum mismatch");
    assert_finding(&pages, "bucket-one/good:Crc32c", "body checksum mismatch");
}
