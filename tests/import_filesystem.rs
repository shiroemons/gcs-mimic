#[allow(dead_code)]
mod common;

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
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::Stdio,
    time::{Duration, UNIX_EPOCH},
};

const ATTRS: &[u8] = include_bytes!("fixtures/import/object-attrs.json");
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

fn execute(options: Options) -> Result<import::Summary, Box<dyn std::error::Error>> {
    import::execute(options, Vec::new(), Vec::new())
}

struct Interrupt {
    remaining: usize,
}
impl Write for Interrupt {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.contains(&b'\n') {
            self.remaining -= 1;
            if self.remaining == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "interrupted report",
                ));
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

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
        assert!(!record.event_based_hold);
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
        assert!(later.event_based_hold);
        let mut config = common::http_config();
        config.data_dir = options.destination.clone();
        config.seed_dir = Some(options.source.join("does-not-exist"));
        assert!(matches!(
            gcs_mimic::seed::import(&catalog, &config, &gcs_mimic::clock::SystemClock)
                .await
                .unwrap(),
            gcs_mimic::seed::SeedOutcome::Skipped
        ));
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
    File::open(&replacement)
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
                File::open(&replacement)?.set_times(FileTimes::new().set_modified(modified))?;
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
    File::open(&body)
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
    assert!(execute(foreign).is_err());
    assert_eq!(execute(options).unwrap().bytes_hashed, 5);
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
fn filesystem_import_preflight_blocks_unsupported_metadata_without_creating_destination() {
    for (field, value) in [
        ("ACL", json!([{"Entity":"allUsers","Role":"READER"}])),
        ("StorageClass", json!("NEARLINE")),
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
    let mut response = Vec::new();
    socket.read_to_end(&mut response).unwrap();
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
