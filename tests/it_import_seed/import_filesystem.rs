use crate::common;
use common::http::Body;

use common::TestDir;
use gcs_mimic::{
    import::{self, Fingerprint, MetadataMode, Options, SourcePlatform},
    storage::{
        Catalog,
        admin::{BucketName, ObjectName, Origin},
    },
};
use serde_json::{Value, json};
use std::{
    fs::{self, File, FileTimes},
    io::{self, BufRead, BufReader, Write},
    net::TcpStream,
    num::NonZeroUsize,
    path::Path,
    process::Stdio,
    time::{Duration, UNIX_EPOCH},
};

const ATTRS: &[u8] = include_bytes!("../fixtures/import/object-attrs.json");
const GENERATION: i64 = 1_792_800_000_123_456;

fn fixture() -> (TestDir, Options) {
    let dir = TestDir::new();
    let source = dir.0.join("source");
    fs::create_dir_all(source.join("bucket-one")).unwrap();
    fs::create_dir_all(source.join("empty-bucket")).unwrap();
    fs::write(
        source.join("bucket-one.bucketMetadata"),
        br#"{"DefaultEventBasedHold":true}"#,
    )
    .unwrap();
    for name in ["a", "b"] {
        fs::write(source.join("bucket-one").join(name), b"hello").unwrap();
        fs::write(
            source.join("bucket-one").join(format!("{name}.metadata")),
            ATTRS,
        )
        .unwrap();
    }
    let options = Options::new(
        source,
        dir.0.join("destination"),
        MetadataMode::Sidecar,
        SourcePlatform::Unix,
    );
    (dir, options)
}

fn execute(options: Options) -> Result<import::Summary, import::ImportError> {
    import::execute(options, Vec::new(), Vec::new())
}

use common::interrupt::Interrupt;

fn interrupted(options: &Options) {
    let result = import::execute(options.clone(), Interrupt { remaining: 2 }, Vec::new());
    assert!(result.is_err());
    assert_eq!(
        fs::read_dir(options.destination.join("staging"))
            .unwrap()
            .count(),
        0
    );
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn filesystem_import_preserves_source_identity_and_skips_later_seed() {
    let (_dir, options) = fixture();
    let body = options.source.join("bucket-one/a");
    let sidecar = options.source.join("bucket-one/a.metadata");
    let before = Fingerprint::of(&fs::metadata(&body).unwrap()).unwrap();
    let attrs_before = Fingerprint::of(&fs::metadata(&sidecar).unwrap()).unwrap();
    let summary = execute(options.clone()).unwrap();
    assert!(summary.complete && summary.eligible && summary.bodies_verified);
    assert_eq!((summary.buckets, summary.objects), (2, 2));
    assert_eq!(summary.bytes_hashed, 10);
    assert_eq!(
        before,
        Fingerprint::of(&fs::metadata(&body).unwrap()).unwrap()
    );
    assert_eq!(
        attrs_before,
        Fingerprint::of(&fs::metadata(&sidecar).unwrap()).unwrap()
    );
    assert_eq!(fs::read(&body).unwrap(), b"hello");
    assert_eq!(fs::read(&sidecar).unwrap(), ATTRS);
    runtime().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        assert!(!catalog.import_journal().in_progress().await.unwrap());
        let admin = catalog.admin(Origin::Import);
        let bucket = BucketName::new("bucket-one").unwrap();
        assert!(
            admin
                .get_bucket(&bucket)
                .await
                .unwrap()
                .unwrap()
                .default_event_based_hold
        );
        assert!(
            admin
                .get_bucket(&BucketName::new("empty-bucket").unwrap())
                .await
                .unwrap()
                .is_some()
        );
        let (record, mut reader) = admin
            .open_object(&bucket, &ObjectName::new("a").unwrap(), None)
            .await
            .unwrap();
        assert_eq!(record.generation, GENERATION);
        assert_eq!(record.metageneration, 1);
        assert_eq!(record.time_created, UNIX_EPOCH - Duration::from_micros(1));
        assert_eq!(
            record.updated,
            UNIX_EPOCH + Duration::from_micros(1_792_800_000_123_456)
        );
        assert_eq!(record.content_type, "text/plain");
        assert_eq!(record.crc32c, crc32c::crc32c(b"hello"));
        {
            use md5::{Digest, Md5};
            assert_eq!(record.md5, <[u8; 16]>::from(Md5::digest(b"hello")));
        }
        assert_eq!(record.content_disposition.as_deref(), Some("inline"));
        assert_eq!(
            record.metadata.get("source").map(String::as_str),
            Some("fixture")
        );
        assert_eq!(record.event_based_hold.as_bool(), None);
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), &b"hello"[..]);
        assert!(reader.next_chunk().await.unwrap().is_none());
        let later = admin
            .publish_from_file(
                &bucket,
                &ObjectName::new("later").unwrap(),
                gcs_mimic::storage::admin::Publication {
                    content_type: "application/octet-stream".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: Some(gcs_mimic::storage::admin::GenerationCondition::Absent),
                    max_bytes: 1024,
                    mode: gcs_mimic::storage::admin::PublishMode::Replace,
                },
                File::open(&body).unwrap(),
            )
            .await
            .unwrap();
        assert!(later.generation > GENERATION);
        assert_eq!(later.event_based_hold.as_bool(), Some(true));
        // Seed replays on imported catalogs too: fixtures without a matching file leave every
        // imported object and its source generation untouched.
        let mut config = common::http_config();
        config.data_dir = options.destination.clone();
        let empty_fixture = options.destination.parent().unwrap().join("empty-seed");
        fs::create_dir(&empty_fixture).unwrap();
        config.seed_dir = Some(empty_fixture);
        assert!(matches!(
            gcs_mimic::seed::import(&catalog, &config, &gcs_mimic::clock::SystemClock)
                .await
                .unwrap(),
            gcs_mimic::seed::SeedOutcome::Imported {
                buckets: 0,
                objects: 0,
                replaced: 0,
                unchanged: 0,
                blocked: 0,
                skipped_os_metadata: 0,
            }
        ));
        let replayed = admin
            .get_object(&bucket, &ObjectName::new("a").unwrap(), None)
            .await
            .unwrap();
        assert_eq!(replayed.generation, GENERATION);
        assert_eq!(replayed.metageneration, 1);
        assert_eq!(replayed.content_type, "text/plain");
        assert_eq!(
            admin
                .get_object(&bucket, &ObjectName::new("later").unwrap(), None)
                .await
                .unwrap()
                .generation,
            later.generation
        );
        config.seed_dir = Some(options.source.join("does-not-exist"));
        assert!(
            gcs_mimic::seed::import(&catalog, &config, &gcs_mimic::clock::SystemClock)
                .await
                .is_err(),
            "a configured but missing seed root is an error, not a silent skip"
        );
        catalog.close().await.unwrap();
    });
}

#[test]
fn filesystem_import_interrupt_resume_and_serve_gate() {
    let (_dir, options) = fixture();
    interrupted(&options);
    let stopped = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(&options.destination)
        .output()
        .unwrap();
    assert!(!stopped.status.success());
    assert!(stopped.stdout.is_empty());
    assert!(String::from_utf8_lossy(&stopped.stderr).contains("Import is incomplete"));
    assert_eq!(execute(options.clone()).unwrap().bytes_hashed, 5);
    runtime().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        let admin = catalog.admin(Origin::Import);
        let page = admin
            .list_objects(
                &BucketName::new("bucket-one").unwrap(),
                Default::default(),
                256,
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(
            page.items
                .iter()
                .all(|record| record.generation == GENERATION)
        );
        catalog.close().await.unwrap();
    });
}

#[cfg(unix)]
#[test]
fn filesystem_import_resume_revalidates_inode_and_ctime_only_drift() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, options) = fixture();
    interrupted(&options);
    let body = options.source.join("bucket-one/a");
    let modified = fs::metadata(&body).unwrap().modified().unwrap();
    let replacement = options.source.join("replacement");
    fs::write(&replacement, b"hello").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    fs::rename(&replacement, &body).unwrap();
    fs::set_permissions(&body, fs::Permissions::from_mode(0o600)).unwrap();
    let mut diagnostics = Vec::new();
    import::execute(options, Vec::new(), &mut diagnostics).unwrap();
    assert!(
        String::from_utf8(diagnostics)
            .unwrap()
            .contains("stat_identity_drift")
    );
}

#[test]
fn filesystem_import_source_replacement_during_publication_stays_unservable() {
    struct ReplaceAfterBucket {
        body: std::path::PathBuf,
        replaced: bool,
    }
    impl Write for ReplaceAfterBucket {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.contains(&b'\n') && !self.replaced {
                self.replaced = true;
                let modified = fs::metadata(&self.body)?.modified()?;
                let replacement = self.body.with_extension("replacement");
                fs::write(&replacement, b"hello")?;
                fs::OpenOptions::new()
                    .write(true)
                    .open(&replacement)?
                    .set_times(FileTimes::new().set_modified(modified))?;
                fs::rename(replacement, &self.body)?;
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (_dir, options) = fixture();
    let output = ReplaceAfterBucket {
        body: options.source.join("bucket-one/a"),
        replaced: false,
    };
    assert!(import::execute(options.clone(), output, Vec::new()).is_err());
    runtime().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        assert!(catalog.import_journal().in_progress().await.unwrap());
        catalog.close().await.unwrap();
    });
    assert!(execute(options).unwrap().bodies_verified);
}

#[test]
fn filesystem_import_checksum_failure_is_terminal_even_after_source_repair() {
    let (_dir, options) = fixture();
    let body = options.source.join("bucket-one/b");
    fs::write(&body, b"world").unwrap();
    let modified = fs::metadata(&body).unwrap().modified().unwrap();
    assert!(execute(options.clone()).is_err());
    assert_eq!(
        fs::read_dir(options.destination.join("staging"))
            .unwrap()
            .count(),
        0
    );
    fs::write(&body, b"hello").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&body)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(
        execute(options.clone()).is_err(),
        "a repaired source must not resurrect terminal import state"
    );
    runtime().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        assert!(catalog.import_journal().in_progress().await.unwrap());
        catalog.close().await.unwrap();
    });
}

#[test]
fn filesystem_import_foreign_options_refuse_without_poisoning_original_resume() {
    let (_dir, options) = fixture();
    interrupted(&options);
    let mut foreign = options.clone();
    foreign.max_object_bytes -= 1;
    let failure = execute(foreign).unwrap_err().to_string();
    assert!(failure.contains("--max-object-bytes"), "{failure}");
    let mut foreign = options.clone();
    foreign.max_entries = NonZeroUsize::new(1000);
    let failure = execute(foreign).unwrap_err().to_string();
    assert!(failure.contains("--max-entries"), "{failure}");
    let mut foreign = options.clone();
    foreign.max_depth = NonZeroUsize::new(100);
    let failure = execute(foreign).unwrap_err().to_string();
    assert!(failure.contains("--max-depth"), "{failure}");
    assert_eq!(execute(options).unwrap().bytes_hashed, 5);
}

/// An earlier release recorded its defaults of 1 GiB, 10,000 entries and 32 levels in the identity
/// of an import that named no limits, so options naming none must still find that import. Options
/// that name other limits are not asking for it.
#[test]
fn an_import_an_earlier_release_started_with_its_defaults_resumes_without_limit_options() {
    let (_dir, options) = fixture();
    let mut former = options.clone();
    former.max_object_bytes = 1_073_741_824;
    former.max_entries = NonZeroUsize::new(10_000);
    former.max_depth = NonZeroUsize::new(32);
    interrupted(&former);
    let mut named = options.clone();
    named.max_entries = NonZeroUsize::new(20_000);
    let failure = execute(named).unwrap_err().to_string();
    assert!(failure.contains("--max-entries"), "{failure}");
    assert_eq!(execute(options.clone()).unwrap().bytes_hashed, 5);
    assert_eq!(served_objects(&options, "bucket-one"), 2);
}

fn cli(options: &Options, limits: &[&str]) -> std::process::Output {
    common::server_command()
        .args(["import", "fake-gcs-server", "--source"])
        .arg(&options.source)
        .arg("--data-dir")
        .arg(&options.destination)
        .args(["--metadata-mode", "sidecar", "--source-platform", "unix"])
        .args(limits)
        .output()
        .unwrap()
}

/// Asserts a failed run names an option in its diagnostics and leaves nothing to resume.
fn refuses(options: &Options, limits: &[&str], option: &str) {
    let output = cli(options, limits);
    assert!(!output.status.success());
    let report = [output.stdout, output.stderr].concat();
    let report = String::from_utf8_lossy(&report);
    assert!(report.contains(option), "{limits:?}: {report}");
    assert!(!options.destination.exists(), "{limits:?}");
}

fn served_objects(options: &Options, bucket: &str) -> usize {
    runtime().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        let page = catalog
            .admin(Origin::Import)
            .list_objects(&BucketName::new(bucket).unwrap(), Default::default(), 256)
            .await
            .unwrap();
        catalog.close().await.unwrap();
        page.items.len()
    })
}

/// A 5-byte body at a 4-byte limit stands in for a body past a limit of 1 GiB: the same cap decides
/// both, and a real one would spend the test's time hashing it twice.
#[test]
fn the_object_size_option_is_inclusive_and_reaches_the_catalog() {
    let (_dir, options) = fixture();
    refuses(
        &options,
        &["--dry-run", "--max-object-bytes", "4"],
        "--max-object-bytes",
    );
    let output = cli(&options, &["--max-object-bytes", "5"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(served_objects(&options, "bucket-one"), 2);
}

#[test]
fn the_default_object_size_is_cloud_storages_own_and_a_named_limit_stops_short_of_it() {
    const ONE_GIB: u64 = 1_073_741_824;
    const FIVE_TIB: u64 = 5 * 1024 * 1024 * 1024 * 1024;
    let (_dir, options) = fixture();
    // Sparse bodies: a dry run compares their sizes and reads none of their bytes.
    let body = File::options()
        .write(true)
        .open(options.source.join("bucket-one/a"))
        .unwrap();
    let succeeds = |limits: &[&str]| {
        let output = cli(&options, limits);
        assert!(
            output.status.success(),
            "{limits:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    body.set_len(ONE_GIB + 1).unwrap();
    succeeds(&["--dry-run"]);
    succeeds(&["--dry-run", "--max-object-bytes", "0"]);
    let named = ONE_GIB.to_string();
    refuses(
        &options,
        &["--dry-run", "--max-object-bytes", &named],
        "--max-object-bytes",
    );
    let raised = (ONE_GIB + 1).to_string();
    succeeds(&["--dry-run", "--max-object-bytes", &raised]);
    // Not every test filesystem can address a sparse file this long.
    if body.set_len(FIVE_TIB + 1).is_ok() {
        refuses(&options, &["--dry-run"], "--max-object-bytes");
    }
}

#[test]
fn the_entry_and_depth_options_are_inclusive_and_their_failures_name_them() {
    let (_dir, options) = fixture();
    let nested = options
        .source
        .join("bucket-one")
        .join(std::iter::repeat_n("d", 33).collect::<std::path::PathBuf>());
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("deep"), b"hello").unwrap();
    fs::write(nested.join("deep.metadata"), ATTRS).unwrap();
    // Seven entries of the fixture, 33 directories and the two files inside the deepest.
    const ENTRIES: &str = "42";
    refuses(
        &options,
        &["--dry-run", "--max-depth", "32", "--max-entries", ENTRIES],
        "--max-depth",
    );
    refuses(
        &options,
        &["--dry-run", "--max-depth", "33", "--max-entries", "41"],
        "--max-entries",
    );
    refuses(&options, &["--dry-run", "--max-depth", "32"], "--max-depth");
    let output = cli(&options, &["--max-depth", "33", "--max-entries", ENTRIES]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(served_objects(&options, "bucket-one"), 3);
}

/// Past the former defaults of 10,000 entries and 32 levels, with no option naming a limit. The
/// entries take a dry run, since a full import would fsync every one of them.
#[test]
fn a_source_past_the_former_entry_and_depth_defaults_needs_no_option() {
    let (_dir, options) = fixture();
    let nested = options
        .source
        .join("bucket-one")
        .join(std::iter::repeat_n("d", 33).collect::<std::path::PathBuf>());
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("deep"), b"hello").unwrap();
    fs::write(nested.join("deep.metadata"), ATTRS).unwrap();
    let output = cli(&options, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(served_objects(&options, "bucket-one"), 3);

    let (_dir, options) = fixture();
    let many = options.source.join("bucket-one/many");
    fs::create_dir(&many).unwrap();
    for index in 0..5001 {
        fs::write(many.join(format!("{index:05}")), b"hello").unwrap();
        fs::write(many.join(format!("{index:05}.metadata")), ATTRS).unwrap();
    }
    let output = cli(&options, &["--dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!options.destination.exists());
}

#[test]
fn limit_options_reject_values_out_of_range_and_repeats() {
    let (_dir, options) = fixture();
    for (option, bad, maximum) in [
        ("--max-object-bytes", "5497558138881", "5497558138880"),
        (
            "--max-entries",
            "18446744073709551616",
            "18446744073709551615",
        ),
        ("--max-depth", "1025", "1024"),
    ] {
        for value in [bad, "many", "-1"] {
            let output = cli(&options, &[option, value]);
            assert!(!output.status.success(), "{option} {value}");
            let report = String::from_utf8_lossy(&output.stderr).into_owned();
            assert!(report.contains(option), "{option} {value}: {report}");
            if value != "-1" {
                assert!(report.contains(maximum), "{option} {value}: {report}");
            }
        }
        let output = cli(&options, &[option, "7", option, "7"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate"));
    }
    assert!(!options.destination.exists());
    let help = common::server_command()
        .args(["import", "fake-gcs-server", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    for option in ["--max-object-bytes", "--max-entries", "--max-depth"] {
        assert!(help.contains(option), "{help}");
    }
}

/// The CLI path to the same identity check: an unfinished import started with the defaults refuses
/// other limits, names them and still resumes with the defaults, whether or not it spells them.
#[test]
fn a_resume_with_other_limits_names_them_and_the_defaults_still_resume() {
    let (_dir, options) = fixture();
    interrupted(&options);
    for limits in [
        ["--max-object-bytes", "1073741825"],
        ["--max-entries", "10001"],
        ["--max-depth", "33"],
    ] {
        let output = cli(&options, &limits);
        assert!(!output.status.success(), "{limits:?}");
        let report = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(report.contains(limits[0]), "{limits:?}: {report}");
    }
    let same = cli(
        &options,
        &[
            "--max-object-bytes",
            "0",
            "--max-entries",
            "0",
            "--max-depth",
            "0",
        ],
    );
    assert!(
        same.status.success(),
        "{}",
        String::from_utf8_lossy(&same.stderr)
    );
    assert_eq!(served_objects(&options, "bucket-one"), 2);
}

/// Spelling out the former defaults names limits, so it resumes what they identify as well.
#[test]
fn a_resume_that_repeats_the_former_defaults_finds_their_import() {
    let (_dir, options) = fixture();
    let mut former = options.clone();
    former.max_object_bytes = 1_073_741_824;
    former.max_entries = NonZeroUsize::new(10_000);
    former.max_depth = NonZeroUsize::new(32);
    interrupted(&former);
    let same = cli(
        &options,
        &[
            "--max-object-bytes",
            "1073741824",
            "--max-entries",
            "10000",
            "--max-depth",
            "32",
        ],
    );
    assert!(
        same.status.success(),
        "{}",
        String::from_utf8_lossy(&same.stderr)
    );
    assert_eq!(served_objects(&options, "bucket-one"), 2);
}

#[test]
fn filesystem_import_rejects_destination_records_outside_its_prefix() {
    let (_dir, options) = fixture();
    interrupted(&options);
    runtime().block_on(async {
        let catalog = Catalog::open(&options.destination).await.unwrap();
        let admin = catalog.admin(Origin::Import);
        let name = BucketName::new("unrelated-bucket").unwrap();
        let definition = gcs_mimic::storage::admin::BucketDefinition::default_for(
            &name,
            "other-project",
            &gcs_mimic::clock::SystemClock,
        )
        .unwrap();
        admin.ensure_bucket(definition).await.unwrap();
        catalog.close().await.unwrap();
    });
    assert!(execute(options).is_err());
}

#[test]
fn filesystem_import_changed_export_is_terminal_after_source_repair() {
    let (_dir, options) = fixture();
    interrupted(&options);
    let attrs_path = options.source.join("bucket-one/b.metadata");
    let mut attrs: Value = serde_json::from_slice(ATTRS).unwrap();
    attrs["Metadata"]["new"] = json!("value");
    fs::write(&attrs_path, serde_json::to_vec(&attrs).unwrap()).unwrap();
    assert!(execute(options.clone()).is_err());
    fs::write(attrs_path, ATTRS).unwrap();
    assert!(
        execute(options).is_err(),
        "source repair must not clear terminal export drift"
    );
}

#[test]
fn filesystem_import_preserves_classes_and_custom_time_through_resume_and_restart() {
    for class in [
        "STANDARD",
        "NEARLINE",
        "COLDLINE",
        "ARCHIVE",
        "MULTI_REGIONAL",
        "DURABLE_REDUCED_AVAILABILITY",
    ] {
        let (_dir, options) = fixture();
        let sidecar = options.source.join("bucket-one/a.metadata");
        let mut attrs: Value = serde_json::from_slice(ATTRS).unwrap();
        attrs["StorageClass"] = json!(class);
        attrs["CustomTime"] = json!("2026-01-02T03:04:05.123456+09:00");
        let bytes = serde_json::to_vec(&attrs).unwrap();
        fs::write(&sidecar, &bytes).unwrap();
        let before = Fingerprint::of(&fs::metadata(&sidecar).unwrap()).unwrap();
        assert!(import::execute(options.clone(), Interrupt { remaining: 3 }, Vec::new()).is_err());
        assert!(execute(options.clone()).unwrap().complete);
        for _ in 0..2 {
            runtime().block_on(async {
                let catalog = Catalog::open(&options.destination).await.unwrap();
                let object = catalog
                    .admin(Origin::Import)
                    .get_object(
                        &BucketName::new("bucket-one").unwrap(),
                        &ObjectName::new("a").unwrap(),
                        None,
                    )
                    .await
                    .unwrap();
                assert_eq!(object.storage_class.as_str(), class);
                assert_eq!(
                    object.custom_time.as_deref(),
                    Some("2026-01-02T03:04:05.123456+09:00")
                );
                assert_eq!(object.generation, GENERATION);
                catalog.close().await.unwrap();
            });
        }
        assert_eq!(fs::read(&sidecar).unwrap(), bytes);
        assert_eq!(
            Fingerprint::of(&fs::metadata(&sidecar).unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn filesystem_import_preflight_blocks_unsupported_metadata_without_creating_destination() {
    for (field, value) in [
        ("ACL", json!([{"Entity":"allUsers","Role":"WRITER"}])),
        ("StorageClass", json!("UNKNOWN")),
        ("Created", json!("2026-10-24T00:00:00.123456001Z")),
    ] {
        let (_dir, options) = fixture();
        let mut attrs: Value = serde_json::from_slice(ATTRS).unwrap();
        attrs[field] = value;
        fs::write(
            options.source.join("bucket-one/b.metadata"),
            serde_json::to_vec(&attrs).unwrap(),
        )
        .unwrap();
        assert!(execute(options.clone()).is_err());
        assert!(!options.destination.exists());
    }
}

fn request(address: &str, path: &str, token: bool) -> (u16, Vec<u8>) {
    let mut socket = TcpStream::connect(address).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let authorization = if token {
        "Authorization: Bearer local-admin-token\r\n"
    } else {
        ""
    };
    write!(
        socket,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}\r\n"
    )
    .unwrap();
    let response =
        common::read_http_response(&mut std::io::BufReader::new(socket), Body::Expected).unwrap();
    let end = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap()
        + 4;
    let status = std::str::from_utf8(&response[..end])
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, response[end..].to_vec())
}

#[test]
fn imported_bucket_responses_preserve_absent_and_explicit_false_versioning() {
    let (_dir, options) = fixture();
    fs::create_dir(options.source.join("disabled-bucket")).unwrap();
    fs::write(
        options.source.join("disabled-bucket.bucketMetadata"),
        br#"{"VersioningEnabled":false}"#,
    )
    .unwrap();
    execute(options.clone()).unwrap();
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(&options.destination)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    let address = ready["listen"].as_str().unwrap();
    for (bucket, configured) in [
        ("bucket-one", false),
        ("empty-bucket", false),
        ("disabled-bucket", true),
    ] {
        let (status, body) = request(address, &format!("/storage/v1/b/{bucket}"), false);
        assert_eq!(status, 200);
        let resource: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            resource.get("versioning"),
            configured.then(|| json!({"enabled":false})).as_ref()
        );
    }
    common::stop_server(&mut child, &mut stdout);
}

#[test]
fn filesystem_import_generation_urls_conditions_and_strict_permissions() {
    for strict in [false, true] {
        let (_dir, options) = fixture();
        execute(options.clone()).unwrap();
        let mut command = common::server_command();
        command
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&options.destination);
        if strict {
            command.args([
                "--auth-mode",
                "strict",
                "--identity-file",
                "config/identities.json",
            ]);
        }
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        let address = ready["listen"].as_str().unwrap();
        let path = format!("/storage/v1/b/bucket-one/o/a?alt=media&generation={GENERATION}");
        assert_eq!(
            request(address, &path, false).0,
            if strict { 401 } else { 200 }
        );
        assert_eq!(request(address, &path, strict), (200, b"hello".to_vec()));
        assert_eq!(
            request(
                address,
                &format!("{path}&ifGenerationMatch={GENERATION}"),
                strict
            )
            .0,
            200
        );
        assert_eq!(
            request(address, &format!("{path}&ifGenerationMatch=1"), strict).0,
            412
        );
        assert_eq!(
            request(
                address,
                "/storage/v1/b/bucket-one/o/a?alt=media&generation=1",
                strict
            )
            .0,
            404
        );
        common::stop_server(&mut child, &mut stdout);
    }
}

#[test]
fn filesystem_import_cli_creates_catalog_without_starting_server() {
    let (_dir, options) = fixture();
    let result = common::server_command()
        .args(["import", "fake-gcs-server", "--source"])
        .arg(&options.source)
        .arg("--data-dir")
        .arg(&options.destination)
        .args(["--metadata-mode", "sidecar", "--source-platform", "unix"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("\"event\":\"import_complete\""));
    assert!(!output.contains("\"event\":\"ready\""));
    assert!(
        Path::new(&options.destination)
            .join("catalog.sqlite3")
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn filesystem_import_limits_open_source_files_under_a_low_descriptor_limit() {
    let (_dir, options) = fixture();
    for number in 0..300 {
        let name = format!("body-{number:03}");
        fs::write(options.source.join("bucket-one").join(&name), b"hello").unwrap();
        fs::write(
            options
                .source
                .join("bucket-one")
                .join(format!("{name}.metadata")),
            ATTRS,
        )
        .unwrap();
    }
    let result = std::process::Command::new("sh")
        .env_clear()
        .args(["-c", "ulimit -n 64 && exec \"$@\"", "import-low-fd"])
        .arg(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(["import", "fake-gcs-server", "--source"])
        .arg(&options.source)
        .arg("--data-dir")
        .arg(&options.destination)
        .args(["--metadata-mode", "sidecar", "--source-platform", "unix"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let summary: Value = serde_json::from_slice(
        result
            .stdout
            .split(|byte| *byte == b'\n')
            .rfind(|line| !line.is_empty())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(summary["event"], "import_complete");
    assert_eq!(summary["summary"]["objects"], 302);
}
