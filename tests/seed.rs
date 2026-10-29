mod common;

use common::TestDir;
use gcs_mimic::storage::Catalog;
use gcs_mimic::storage::admin::{BucketName, ObjectName, Origin};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
    time::Duration,
};

fn command(data: &Path, seed: Option<&Path>) -> Command {
    let mut command = common::server_command();
    command
        .env("GCS_MIMIC_LISTEN", "127.0.0.1:0")
        .args(["serve", "--scheme", "http", "--data-dir"])
        .arg(data);
    if let Some(seed) = seed {
        command.arg("--seed-dir").arg(seed);
    }
    command
}

#[cfg(unix)]
fn startup_events(child: &mut Child) -> std::sync::mpsc::Receiver<Value> {
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let event = serde_json::from_str(&line.unwrap()).unwrap();
            if send.send(event).is_err() {
                break;
            }
        }
    });
    receive
}

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
    ready: Value,
    strict: bool,
}

impl Server {
    fn start(command: &mut Command, strict: bool) -> Self {
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready after seeding");
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            stdout,
            address: ready["listen"].as_str().unwrap().into(),
            ready,
            strict,
        }
    }

    fn request(&self, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let auth = if self.strict {
            "Authorization: Bearer local-admin-token\r\n"
        } else {
            ""
        };
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n{auth}\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        let mut reader = BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            headers.push_str(&line);
        }
        let status: u16 = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let length = if status == 204 {
            0
        } else {
            headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap()
        };
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        (status, bytes)
    }

    fn json(&self, path: &str) -> Value {
        let (status, bytes) = self.request("GET", path, b"");
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
        serde_json::from_slice(&bytes).unwrap()
    }

    fn stop(mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

fn fixture(root: &Path, name: &str, bytes: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn fails(command: &mut Command, diagnostic: &str) {
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "failure emitted a ready event");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(diagnostic), "{error}");
}

#[test]
fn seed_configuration_precedence_and_validation() {
    let dir = TestDir::new();
    let file = dir.0.join("settings.json");
    fs::write(
        &file,
        r#"{"seed_dir":"file-seed","default_seed_dir":"file-default","seed_project":"file-project","seed_content_type":"octet-stream"}"#,
    )
    .unwrap();
    let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
    let defaults = common::load_http_config(&[], &BTreeMap::new()).unwrap();
    assert_eq!(defaults.seed_dir, None);
    assert_eq!(defaults.default_seed_dir, None);
    assert_eq!(defaults.seed_project, "local-project");
    assert_eq!(
        defaults.seed_content_type,
        gcs_mimic::mime::SeedContentType::Extension
    );
    let loaded = common::load_http_config(&args, &BTreeMap::new()).unwrap();
    assert_eq!(loaded.seed_dir.as_deref(), Some(Path::new("file-seed")));
    assert_eq!(
        loaded.default_seed_dir.as_deref(),
        Some(Path::new("file-default"))
    );
    assert_eq!(loaded.seed_project, "file-project");
    assert_eq!(
        loaded.seed_content_type,
        gcs_mimic::mime::SeedContentType::OctetStream
    );
    let env = BTreeMap::from([
        ("GCS_MIMIC_SEED_DIR".into(), "env-seed".into()),
        ("GCS_MIMIC_DEFAULT_SEED_DIR".into(), "env-default".into()),
        ("GCS_MIMIC_SEED_PROJECT".into(), "env-project".into()),
        ("GCS_MIMIC_SEED_CONTENT_TYPE".into(), "extension".into()),
    ]);
    let loaded = common::load_http_config(&args, &env).unwrap();
    assert_eq!(loaded.seed_dir.as_deref(), Some(Path::new("env-seed")));
    assert_eq!(
        loaded.default_seed_dir.as_deref(),
        Some(Path::new("env-default"))
    );
    assert_eq!(loaded.seed_project, "env-project");
    assert_eq!(
        loaded.seed_content_type,
        gcs_mimic::mime::SeedContentType::Extension
    );
    args.extend([
        "--seed-dir".into(),
        "cli-seed".into(),
        "--default-seed-dir".into(),
        "cli-default".into(),
        "--seed-project".into(),
        "cli-project".into(),
        "--seed-content-type".into(),
        "octet-stream".into(),
    ]);
    let loaded = common::load_http_config(&args, &env).unwrap();
    assert_eq!(loaded.seed_dir.as_deref(), Some(Path::new("cli-seed")));
    assert_eq!(
        loaded.default_seed_dir.as_deref(),
        Some(Path::new("cli-default"))
    );
    assert_eq!(loaded.seed_project, "cli-project");
    assert_eq!(
        loaded.seed_content_type,
        gcs_mimic::mime::SeedContentType::OctetStream
    );
    for (key, invalid) in [
        ("seed_dir", ""),
        ("default_seed_dir", ""),
        ("seed_content_type", ""),
        ("seed_content_type", "unknown"),
        ("seed_content_type", "Extension"),
        ("seed_project", ""),
        ("seed_project", "with space"),
        ("seed_project", "line\nfeed"),
        ("seed_project", &"x".repeat(257)),
    ] {
        let flag = format!("--{}", key.replace('_', "-"));
        assert!(common::load_http_config(&[flag, invalid.into()], &BTreeMap::new()).is_err());
        let env = BTreeMap::from([(
            format!("GCS_MIMIC_{}", key.to_ascii_uppercase()),
            invalid.into(),
        )]);
        assert!(common::load_http_config(&[], &env).is_err());
        fs::write(&file, json!({key:invalid}).to_string()).unwrap();
        assert!(common::load_http_config(&args[..2], &BTreeMap::new()).is_err());
    }
}

#[test]
fn seeded_http_metadata_bytes_and_strict_administrative_import() {
    for strict in [false, true] {
        let dir = TestDir::new();
        let seed = dir.0.join("fixtures");
        let data = dir.0.join("data");
        fixture(&seed, "local-bucket/dir/hello.txt", b"hello");
        fixture(&seed, "local-bucket/.gitkeep", b"");
        fixture(&seed, "local-bucket/.DS_Store", b"hidden");
        fixture(&seed, ".DS_Store", b"root metadata");
        fixture(&seed, "._bucket", b"root metadata");
        fixture(&seed, "local-bucket/dir/._hello.txt", b"nested metadata");
        fixture(&seed, "local-bucket/._assets/kept", b"real input");
        fixture(&seed, "local-bucket/._assets/.DS_Store", b"deep metadata");
        fixture(&seed, "local-bucket/space % 雪.bin", b"\0\xff");
        let large = vec![42; 131_073];
        fixture(&seed, "local-bucket/large.bin", &large);
        fs::create_dir_all(seed.join("empty-bucket")).unwrap();
        let mut cmd = command(&data, Some(&seed));
        if strict {
            cmd.args([
                "--auth-mode",
                "strict",
                "--identity-file",
                "config/identities.json",
            ]);
        }
        let server = Server::start(&mut cmd, strict);
        assert_eq!(
            server.ready["seed"],
            json!({"outcome":"imported","buckets":2,"objects":5,"replaced":0,"unchanged":0,"blocked":0,"skipped_os_metadata":5})
        );
        assert_eq!(server.request("GET", "/_internal/ready", b"").0, 200);
        let buckets = server.json("/storage/v1/b?project=local-project");
        assert_eq!(buckets["items"].as_array().unwrap().len(), 2);
        assert_eq!(
            server.json("/storage/v1/b/local-bucket/o")["items"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        assert!(
            server.json("/storage/v1/b/empty-bucket/o")["items"]
                .as_array()
                .is_none_or(|items| items.is_empty())
        );
        let metadata = server.json("/storage/v1/b/local-bucket/o/dir%2Fhello.txt");
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/local-bucket/o/.DS_Store", b"")
                .0,
            404
        );
        assert_eq!(
            server.request(
                "GET",
                "/storage/v1/b/local-bucket/o/._assets%2Fkept?alt=media",
                b""
            ),
            (200, b"real input".to_vec())
        );
        assert_eq!(metadata["size"], "5");
        assert_eq!(metadata["md5Hash"], "XUFAKrxLKna5cZ2REBfFkg==");
        assert_eq!(metadata["crc32c"], "mnG7TA==");
        assert_eq!(metadata["contentType"], "text/plain; charset=utf-8");
        assert_eq!(
            server.json("/storage/v1/b/local-bucket/o/large.bin")["contentType"],
            "application/octet-stream"
        );
        assert_eq!(
            server.request(
                "GET",
                "/storage/v1/b/local-bucket/o/dir%2Fhello.txt?alt=media",
                b""
            ),
            (200, b"hello".to_vec())
        );
        assert_eq!(
            server.request(
                "GET",
                "/storage/v1/b/local-bucket/o/large.bin?alt=media",
                b""
            ),
            (200, large)
        );
        assert_eq!(
            server.request(
                "GET",
                "/storage/v1/b/local-bucket/o/space%20%25%20%E9%9B%AA.bin?alt=media",
                b""
            ),
            (200, b"\0\xff".to_vec())
        );
        server.stop();
    }
}

#[test]
fn extension_mime_is_explicit_and_existing_os_metadata_objects_survive_restart() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let data = dir.0.join("data");
    let cases = [
        ("NOTE.TXT", "text/plain; charset=utf-8"),
        (".yaml", "application/x-yaml"),
        ("style.css", "text/css; charset=utf-8"),
        ("data.json", "application/json"),
        ("archive.tar.gz", "application/gzip"),
        ("foo.", "application/octet-stream"),
        ("literal%2F.bin", "application/octet-stream"),
    ];
    // Windows strips a trailing dot from file names, so "foo." cannot be a fixture there.
    let cases: Vec<_> = cases
        .into_iter()
        .filter(|(name, _)| !(cfg!(windows) && name.ends_with('.')))
        .collect();
    for &(name, _) in &cases {
        fixture(&seed, &format!("local-bucket/{name}"), b"fixture");
    }
    let seeded = cases.len();
    let before = fs::metadata(seed.join("local-bucket/NOTE.TXT")).unwrap();
    let server = Server::start(
        command(&data, Some(&seed)).args(["--seed-content-type", "extension"]),
        false,
    );
    for (name, expected) in cases {
        let encoded = name.replace('%', "%25");
        assert_eq!(
            server.json(&format!("/storage/v1/b/local-bucket/o/{encoded}"))["contentType"],
            expected
        );
        assert_eq!(
            fs::read(seed.join("local-bucket").join(name)).unwrap(),
            b"fixture"
        );
    }
    let after = fs::metadata(seed.join("local-bucket/NOTE.TXT")).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(before.permissions(), after.permissions());
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/local-bucket/o?uploadType=media&name=.DS_Store",
                b"previously imported"
            )
            .0,
        200
    );
    let old = server.json("/storage/v1/b/local-bucket/o/.DS_Store");
    server.stop();
    fixture(&seed, "local-bucket/.DS_Store", b"new metadata");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    // Replay still skips OS metadata files, so the API-written object at that name is not
    // treated as a seeded object and is neither replaced nor counted.
    assert_eq!(
        server.ready["seed"],
        json!({"outcome":"imported","buckets":0,"objects":0,"replaced":0,"unchanged":seeded,"blocked":0,"skipped_os_metadata":1})
    );
    let retained = server.json("/storage/v1/b/local-bucket/o/.DS_Store");
    for field in [
        "generation",
        "metageneration",
        "contentType",
        "size",
        "md5Hash",
        "crc32c",
        "updated",
    ] {
        assert_eq!(retained[field], old[field], "{field}");
    }
    assert_eq!(
        server.request(
            "GET",
            "/storage/v1/b/local-bucket/o/.DS_Store?alt=media",
            b""
        ),
        (200, b"previously imported".to_vec())
    );
    server.stop();
}

#[test]
fn seed_mime_setting_does_not_replace_durable_source_content_type() {
    let dir = TestDir::new();
    let source = dir.0.join("source");
    fixture(&source, "local-bucket/hello.txt", b"hello");
    fixture(
        &source,
        "local-bucket/hello.txt.metadata",
        include_bytes!("fixtures/import/object-attrs.json"),
    );
    let output = common::server_command()
        .env("GCS_MIMIC_SEED_CONTENT_TYPE", "extension")
        .args(["import", "fake-gcs-server", "--source"])
        .arg(&source)
        .arg("--data-dir")
        .arg(dir.0.join("destination"))
        .args([
            "--metadata-mode",
            "sidecar",
            "--source-platform",
            "unix",
            "--dry-run",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pages: Vec<Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let object = pages
        .iter()
        .filter_map(|page| page["page"]["records"].as_array())
        .flatten()
        .find(|record| record["kind"] == "object")
        .unwrap();
    assert_eq!(object["attributes"]["ContentType"], "text/plain");
    assert!(!dir.0.join("destination").exists());
}

fn seed_counts(objects: u64, replaced: u64, unchanged: u64, blocked: u64) -> Value {
    json!({
        "outcome": "imported",
        "buckets": 0,
        "objects": objects,
        "replaced": replaced,
        "unchanged": unchanged,
        "blocked": blocked,
        "skipped_os_metadata": 0,
    })
}

impl Server {
    fn upload(&self, bucket: &str, name: &str, body: &[u8]) -> Value {
        let (status, bytes) = self.request(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}"),
            body,
        );
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
        serde_json::from_slice(&bytes).unwrap()
    }

    fn patch(&self, path: &str, body: &str) -> Value {
        let (status, bytes) = self.request("PATCH", path, body.as_bytes());
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
        serde_json::from_slice(&bytes).unwrap()
    }

    fn delete(&self, path: &str) {
        let (status, bytes) = self.request("DELETE", path, b"");
        assert_eq!(status, 204, "{}", String::from_utf8_lossy(&bytes));
    }

    fn media(&self, path: &str) -> (u16, Vec<u8>) {
        self.request("GET", &format!("{path}?alt=media"), b"")
    }
}

#[test]
fn restart_replays_fixtures_over_api_changes_and_keeps_unrelated_objects() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let data = dir.0.join("data");
    fixture(&seed, "local-bucket/hello.txt", b"fixture");
    fixture(&seed, "local-bucket/keep.txt", b"keep");
    fixture(&seed, "local-bucket/gone.txt", b"gone");
    fixture(&seed, "local-bucket/untouched.txt", b"untouched");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"]["objects"], 4);
    let untouched = server.json("/storage/v1/b/local-bucket/o/untouched.txt");
    server.upload("local-bucket", "hello.txt", b"mutation");
    server.patch(
        "/storage/v1/b/local-bucket/o/hello.txt",
        r#"{"metadata":{"owner":"user"}}"#,
    );
    server.upload("local-bucket", "created.txt", b"new");
    let kept = server.patch(
        "/storage/v1/b/local-bucket/o/keep.txt",
        r#"{"metadata":{"owner":"user"},"contentType":"application/x-custom"}"#,
    );
    server.delete("/storage/v1/b/local-bucket/o/gone.txt");
    server.stop();

    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(1, 2, 1, 0));
    // A fixture file wins over an API overwrite at its name, with seed-default metadata.
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/hello.txt"),
        (200, b"fixture".to_vec())
    );
    let restored = server.json("/storage/v1/b/local-bucket/o/hello.txt");
    assert!(restored["metadata"].is_null(), "{restored}");
    assert_eq!(restored["contentType"], "text/plain; charset=utf-8");
    // Identical bytes with API metadata edits are still reverted to what seeding writes.
    let reverted = server.json("/storage/v1/b/local-bucket/o/keep.txt");
    assert!(reverted["metadata"].is_null(), "{reverted}");
    assert_eq!(reverted["contentType"], "text/plain; charset=utf-8");
    let before: u64 = kept["generation"].as_str().unwrap().parse().unwrap();
    let after: u64 = reverted["generation"].as_str().unwrap().parse().unwrap();
    assert!(after > before, "{before} -> {after}");
    // Exactly what seeding would write mints no generation and keeps every field.
    let unchanged = server.json("/storage/v1/b/local-bucket/o/untouched.txt");
    for field in ["generation", "metageneration", "contentType", "updated"] {
        assert_eq!(unchanged[field], untouched[field], "{field}");
    }
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/gone.txt"),
        (200, b"gone".to_vec())
    );
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/created.txt"),
        (200, b"new".to_vec())
    );
    server.stop();

    // Removing a fixture file never deletes the object it seeded.
    fs::remove_file(seed.join("local-bucket/keep.txt")).unwrap();
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(0, 0, 3, 0));
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/keep.txt"),
        (200, b"keep".to_vec())
    );
    for object in [
        "hello.txt",
        "keep.txt",
        "gone.txt",
        "untouched.txt",
        "created.txt",
    ] {
        server.delete(&format!("/storage/v1/b/local-bucket/o/{object}"));
    }
    server.delete("/storage/v1/b/local-bucket");
    server.stop();

    // A deleted seeded bucket is recreated with only the current fixture files.
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(
        server.ready["seed"],
        json!({"outcome":"imported","buckets":1,"objects":3,"replaced":0,"unchanged":0,"blocked":0,"skipped_os_metadata":0})
    );
    let names: Vec<_> = server.json("/storage/v1/b/local-bucket/o")["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["gone.txt", "hello.txt", "untouched.txt"]);
    server.stop();
}

#[test]
fn edited_fixture_replaces_live_generation_and_versioning_keeps_noncurrent() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let data = dir.0.join("data");
    fixture(&seed, "versioned-bucket/file.txt", b"first");
    fixture(&seed, "plain-bucket/file.txt", b"first");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    server.patch(
        "/storage/v1/b/versioned-bucket",
        r#"{"versioning":{"enabled":true}}"#,
    );
    let versioned = server.json("/storage/v1/b/versioned-bucket/o/file.txt");
    let plain = server.json("/storage/v1/b/plain-bucket/o/file.txt");
    server.stop();

    fixture(&seed, "versioned-bucket/file.txt", b"second");
    fixture(&seed, "plain-bucket/file.txt", b"second");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(0, 2, 0, 0));
    for (bucket, before) in [("versioned-bucket", &versioned), ("plain-bucket", &plain)] {
        let live = server.json(&format!("/storage/v1/b/{bucket}/o/file.txt"));
        let old: u64 = before["generation"].as_str().unwrap().parse().unwrap();
        let new: u64 = live["generation"].as_str().unwrap().parse().unwrap();
        assert!(new > old, "{bucket}: {old} -> {new}");
        assert_eq!(live["size"], "6");
        assert_eq!(
            server.media(&format!("/storage/v1/b/{bucket}/o/file.txt")),
            (200, b"second".to_vec())
        );
        let previous = server.request(
            "GET",
            &format!("/storage/v1/b/{bucket}/o/file.txt?alt=media&generation={old}"),
            b"",
        );
        if bucket == "versioned-bucket" {
            assert_eq!(previous, (200, b"first".to_vec()));
            let versions = server.json(&format!("/storage/v1/b/{bucket}/o?versions=true"));
            assert_eq!(versions["items"].as_array().unwrap().len(), 2);
        } else {
            assert_eq!(previous.0, 404);
        }
    }
    server.stop();
}

#[test]
fn held_object_is_blocked_while_identical_held_object_is_unchanged() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let data = dir.0.join("data");
    fixture(&seed, "local-bucket/held.txt", b"first");
    fixture(&seed, "local-bucket/meta.txt", b"meta");
    fixture(&seed, "local-bucket/same.txt", b"same");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    let [held, _, same] = ["held.txt", "meta.txt", "same.txt"].map(|name| {
        server.patch(
            &format!("/storage/v1/b/local-bucket/o/{name}"),
            r#"{"eventBasedHold":true}"#,
        )
    });
    // Same bytes but API metadata differs from what seeding writes: replacement is needed.
    let meta = server.patch(
        "/storage/v1/b/local-bucket/o/meta.txt",
        r#"{"metadata":{"owner":"user"}}"#,
    );
    server.stop();

    fixture(&seed, "local-bucket/held.txt", b"second");
    let mut server = Server::start(command(&data, Some(&seed)).stderr(Stdio::piped()), false);
    assert_eq!(server.ready["seed"], seed_counts(0, 0, 1, 2));
    for (name, before) in [
        ("held.txt", &held),
        ("meta.txt", &meta),
        ("same.txt", &same),
    ] {
        let current = server.json(&format!("/storage/v1/b/local-bucket/o/{name}"));
        for field in [
            "generation",
            "metageneration",
            "md5Hash",
            "eventBasedHold",
            "metadata",
        ] {
            assert_eq!(current[field], before[field], "{name} {field}");
        }
    }
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/held.txt"),
        (200, b"first".to_vec())
    );
    let stderr = server.child.stderr.take().unwrap();
    server.stop();
    let blocked: Vec<Value> = BufReader::new(stderr)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
        .filter(|event| event["event"] == "seed_blocked")
        .collect();
    let event = |name: &str| {
        let path = Path::new("local-bucket").join(name);
        json!({"event":"seed_blocked","level":"warn","path":path.display().to_string()})
    };
    assert_eq!(blocked, [event("held.txt"), event("meta.txt")]);

    // Releasing the holds lets the next replay publish seed content and metadata.
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(0, 0, 1, 2));
    for name in ["held.txt", "meta.txt"] {
        server.patch(
            &format!("/storage/v1/b/local-bucket/o/{name}"),
            r#"{"eventBasedHold":false}"#,
        );
    }
    server.stop();
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(0, 2, 1, 0));
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/held.txt"),
        (200, b"second".to_vec())
    );
    assert!(server.json("/storage/v1/b/local-bucket/o/meta.txt")["metadata"].is_null());
    server.stop();
}

#[test]
fn default_content_type_is_extension_and_octet_stream_is_opt_in() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let data = dir.0.join("data");
    fixture(&seed, "local-bucket/note.txt", b"text");
    fixture(&seed, "local-bucket/blob.unknown-extension", b"blob");
    let server = Server::start(
        command(&data, Some(&seed)).args(["--seed-content-type", "octet-stream"]),
        false,
    );
    for name in ["note.txt", "blob.unknown-extension"] {
        assert_eq!(
            server.json(&format!("/storage/v1/b/local-bucket/o/{name}"))["contentType"],
            "application/octet-stream"
        );
    }
    server.stop();
    // Switching to the default mode replaces only objects whose seed mapping changes, as
    // happens once for a catalog seeded with the previous octet-stream default.
    fixture(&seed, "local-bucket/new.txt", b"new");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(1, 1, 1, 0));
    for (name, expected) in [
        ("note.txt", "text/plain; charset=utf-8"),
        ("new.txt", "text/plain; charset=utf-8"),
        ("blob.unknown-extension", "application/octet-stream"),
    ] {
        assert_eq!(
            server.json(&format!("/storage/v1/b/local-bucket/o/{name}"))["contentType"],
            expected,
            "{name}"
        );
    }
    let note = server.json("/storage/v1/b/local-bucket/o/note.txt");
    server.stop();
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(0, 0, 3, 0));
    assert_eq!(
        server.json("/storage/v1/b/local-bucket/o/note.txt")["generation"],
        note["generation"]
    );
    server.stop();
    let server = Server::start(
        command(&data, Some(&seed)).args(["--seed-content-type", "octet-stream"]),
        false,
    );
    assert_eq!(server.ready["seed"], seed_counts(0, 2, 1, 0));
    assert_eq!(
        server.json("/storage/v1/b/local-bucket/o/note.txt")["contentType"],
        "application/octet-stream"
    );
    server.stop();
}

fn default_seed_command(data: &Path, default: &Path) -> Command {
    let mut command = command(data, None);
    command.env("GCS_MIMIC_DEFAULT_SEED_DIR", default);
    command
}

#[test]
fn default_seed_dir_imports_only_an_existing_disjoint_directory() {
    let dir = TestDir::new();
    let defaults = dir.0.join("defaults");
    fixture(&defaults, "default-bucket/file.txt", b"default");

    let server = Server::start(
        &mut default_seed_command(&dir.0.join("data"), &defaults),
        false,
    );
    assert_eq!(
        server.ready["seed"],
        json!({"outcome":"imported","buckets":1,"objects":1,"replaced":0,"unchanged":0,"blocked":0,"skipped_os_metadata":0})
    );
    assert_eq!(
        server.media("/storage/v1/b/default-bucket/o/file.txt"),
        (200, b"default".to_vec())
    );
    server.stop();

    let not_directory = dir.0.join("not-directory");
    fs::write(&not_directory, b"file").unwrap();
    let outer = dir.0.join("outer");
    fixture(&outer, "seed/default-bucket/file.txt", b"inside data");
    for (data, default) in [
        (dir.0.join("missing-data"), dir.0.join("missing")),
        (dir.0.join("file-data"), not_directory),
        // Overlap in either direction keeps the data directory's role instead of seeding it.
        (defaults.join("nested-data"), defaults.clone()),
        (outer.clone(), outer.join("seed")),
        (defaults.clone(), defaults.clone()),
    ] {
        let server = Server::start(&mut default_seed_command(&data, &default), false);
        assert_eq!(
            server.ready["seed"],
            json!({"outcome":"disabled"}),
            "{default:?}"
        );
        assert_eq!(
            server.request("GET", "/storage/v1/b/default-bucket", b"").0,
            404
        );
        server.stop();
    }

    let explicit = dir.0.join("explicit");
    fixture(&explicit, "explicit-bucket/file.txt", b"explicit");
    let server = Server::start(
        default_seed_command(&dir.0.join("explicit-data"), &defaults)
            .arg("--seed-dir")
            .arg(&explicit),
        false,
    );
    assert_eq!(server.ready["seed"]["buckets"], 1);
    assert_eq!(
        server.media("/storage/v1/b/explicit-bucket/o/file.txt"),
        (200, b"explicit".to_vec())
    );
    assert_eq!(
        server.request("GET", "/storage/v1/b/default-bucket", b"").0,
        404
    );
    server.stop();
}

#[tokio::test]
async fn interrupted_import_retries_without_overwrite_and_releases_lock() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let data = dir.0.join("data");
    fixture(&seed, "local-bucket/a-valid", b"original");
    fixture(&seed, "z-Invalid/file", b"invalid");
    fails(&mut command(&data, Some(&seed)), "Invalid bucket name");
    let catalog = Catalog::open(&data)
        .await
        .expect("failed seed releases data directory lock");
    let original = catalog
        .admin(Origin::Seed)
        .get_object(
            &BucketName::new("local-bucket").unwrap(),
            &ObjectName::new("a-valid").unwrap(),
            None,
        )
        .await
        .unwrap();
    catalog.close().await.unwrap();
    fails(
        command(&data, Some(&seed)).args(["--seed-project", "other-project"]),
        "belongs to another project",
    );
    fs::remove_dir_all(seed.join("z-Invalid")).unwrap();
    fixture(&seed, "local-bucket/z-valid", b"new");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    // The committed object from the failed attempt matches its fixture, so the retry keeps
    // that generation and only publishes what is still missing.
    assert_eq!(server.ready["seed"], seed_counts(1, 0, 1, 0));
    assert_eq!(
        server.json("/storage/v1/b/local-bucket/o/a-valid")["generation"],
        original.generation.to_string()
    );
    assert_eq!(
        server.request("GET", "/storage/v1/b/local-bucket/o/a-valid?alt=media", b""),
        (200, b"original".to_vec())
    );
    assert_eq!(
        server.request("GET", "/storage/v1/b/local-bucket/o/z-valid?alt=media", b""),
        (200, b"new".to_vec())
    );
    server.stop();
}

#[test]
fn invalid_fixtures_fail_before_ready_and_size_limit_is_inclusive() {
    for (name, diagnostic) in [
        ("Invalid/bad", "Invalid bucket name"),
        #[cfg(unix)]
        ("valid-bucket/bad\n", "Invalid object name"),
        (
            "valid-bucket/.well-known/acme-challenge/token",
            "Invalid object name",
        ),
    ] {
        let dir = TestDir::new();
        let seed = dir.0.join("fixtures");
        fixture(&seed, name, b"bad");
        fails(&mut command(&dir.0.join("data"), Some(&seed)), diagnostic);
    }
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/a-empty", b"");
    fixture(&seed, "local-bucket/b-exact", b"1234");
    fixture(&seed, "local-bucket/z-large", b"12345");
    fails(
        command(&dir.0.join("data"), Some(&seed)).args(["--max-object-bytes", "4"]),
        "max_object_bytes",
    );
    fs::remove_file(seed.join("local-bucket/z-large")).unwrap();
    let server = Server::start(
        command(&dir.0.join("data"), Some(&seed)).args(["--max-object-bytes", "4"]),
        false,
    );
    assert_eq!(
        server.json("/storage/v1/b/local-bucket/o/a-empty")["size"],
        "0"
    );
    assert_eq!(
        server.request("GET", "/storage/v1/b/local-bucket/o/b-exact?alt=media", b""),
        (200, b"1234".to_vec())
    );
    server.stop();
    fails(
        &mut command(&dir.0.join("missing-data"), Some(&dir.0.join("missing"))),
        "seed import:",
    );
    fails(
        &mut command(&seed.join("data"), Some(&seed)),
        "must not overlap",
    );
}

#[test]
fn top_level_files_are_skipped_with_a_warning() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    fixture(&seed, "top.txt", b"not a bucket");
    fixture(&seed, "local-bucket/file", b"fixture");
    let mut server = Server::start(
        command(&dir.0.join("data"), Some(&seed)).stderr(Stdio::piped()),
        false,
    );
    assert_eq!(
        server.ready["seed"],
        json!({"outcome":"imported","buckets":1,"objects":1,"replaced":0,"unchanged":0,"blocked":0,"skipped_os_metadata":0})
    );
    assert_eq!(server.request("GET", "/storage/v1/b/top.txt", b"").0, 404);
    let stderr = server.child.stderr.take().unwrap();
    server.stop();
    let skipped: Vec<Value> = BufReader::new(stderr)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
        .filter(|event| event["event"] == "seed_skipped")
        .collect();
    assert_eq!(
        skipped,
        [
            json!({"event":"seed_skipped","level":"warn","path":"top.txt","reason":"top-level entries must be bucket directories"})
        ]
    );
}

#[test]
fn fixture_depth_and_entry_count_are_bounded() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let deepest = format!("local-bucket/{}file", "d/".repeat(32));
    fixture(&seed, &deepest, b"boundary");
    let server = Server::start(&mut command(&dir.0.join("allowed"), Some(&seed)), false);
    assert_eq!(server.ready["seed"]["objects"], 1);
    server.stop();
    fixture(
        &seed,
        &format!("local-bucket/{}file", "d/".repeat(33)),
        b"too deep",
    );
    fails(
        &mut command(&dir.0.join("deep"), Some(&seed)),
        "32 directory levels",
    );
    let many = dir.0.join("many");
    fs::create_dir(&many).unwrap();
    for index in 0..10_001 {
        fs::write(many.join(format!("entry-{index}")), b"").unwrap();
    }
    fails(
        &mut command(&dir.0.join("many-data"), Some(&many)),
        "10000 entries",
    );
}

#[cfg(unix)]
#[test]
fn symlinks_and_special_files_fail_closed() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"source");
    for name in ["link", ".DS_Store", "._file"] {
        symlink(
            seed.join("local-bucket/file"),
            seed.join("local-bucket").join(name),
        )
        .unwrap();
        fails(
            &mut command(&dir.0.join("link-data"), Some(&seed)),
            "only regular files",
        );
        fs::remove_file(seed.join("local-bucket").join(name)).unwrap();
    }
    let root_link = dir.0.join("root-link");
    symlink(&seed, &root_link).unwrap();
    fails(
        &mut command(&dir.0.join("root-link-data"), Some(&root_link)),
        "not a symlink",
    );
    let socket = seed.join("local-bucket/._socket");
    let _listener = UnixListener::bind(&socket).unwrap();
    fails(
        &mut command(&dir.0.join("socket-data"), Some(&seed)),
        "only regular files",
    );
    fs::remove_file(socket).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_fixture_names_fail_closed() {
    use std::os::unix::ffi::OsStringExt;
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    fs::create_dir_all(seed.join("local-bucket")).unwrap();
    fs::write(
        seed.join("local-bucket")
            .join(std::ffi::OsString::from_vec(vec![0xff])),
        b"invalid",
    )
    .unwrap();
    fails(
        &mut command(&dir.0.join("utf8-data"), Some(&seed)),
        "must be UTF-8",
    );
}

#[test]
fn seed_dir_added_after_disabled_startup_imports_and_removal_disables_again() {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    let server = Server::start(&mut command(&data, None), false);
    assert_eq!(server.ready["seed"], json!({"outcome":"disabled"}));
    server.stop();
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"fixture");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(
        server.ready["seed"],
        json!({"outcome":"imported","buckets":1,"objects":1,"replaced":0,"unchanged":0,"blocked":0,"skipped_os_metadata":0})
    );
    server.upload("local-bucket", "file", b"mutation");
    server.stop();
    // Without seed_dir nothing is replayed, so the API change survives that startup.
    let server = Server::start(&mut command(&data, None), false);
    assert_eq!(server.ready["seed"], json!({"outcome":"disabled"}));
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/file"),
        (200, b"mutation".to_vec())
    );
    server.stop();
}

#[test]
fn explicit_project_and_empty_seed_directory_are_supported() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"fixture");
    let server = Server::start(
        command(&dir.0.join("data"), Some(&seed)).args(["--seed-project", "fixture-project"]),
        false,
    );
    assert_eq!(
        server.json("/storage/v1/b?project=fixture-project")["items"][0]["name"],
        "local-bucket"
    );
    assert!(server.json("/storage/v1/b?project=local-project")["items"].is_null());
    server.stop();
    let empty = dir.0.join("empty");
    fs::create_dir(&empty).unwrap();
    let server = Server::start(&mut command(&dir.0.join("empty-data"), Some(&empty)), false);
    assert_eq!(
        server.ready["seed"],
        json!({"outcome":"imported","buckets":0,"objects":0,"replaced":0,"unchanged":0,"blocked":0,"skipped_os_metadata":0})
    );
    server.stop();
}

#[tokio::test]
async fn populated_v4_catalog_migrates_and_replays_seed() {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"fixture");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    server.upload("local-bucket", "file", b"user");
    server.upload("local-bucket", "api-only", b"api");
    server.stop();
    let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
    common::downgrade_object_schema(&connection);
    connection
        .execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; DROP TABLE bucket_policies; DROP TABLE policy_revision; DROP TABLE startup_seed; PRAGMA user_version = 4;")
        .unwrap();
    drop(connection);
    // A migrated catalog is initialized, but replay still runs: the fixture replaces the API
    // overwrite at its name while the migrated API-only object stays.
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"], seed_counts(0, 1, 0, 0));
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/file"),
        (200, b"fixture".to_vec())
    );
    assert_eq!(
        server.media("/storage/v1/b/local-bucket/o/api-only"),
        (200, b"api".to_vec())
    );
    server.stop();
    let catalog = Catalog::open(&data).await.unwrap();
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        31
    );
}

fn migration_events(data: &Path) -> Vec<Value> {
    let mut server = Server::start(command(data, None).stderr(Stdio::piped()), false);
    let stderr = server.child.stderr.take().unwrap();
    server.stop();
    BufReader::new(stderr)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.unwrap()).unwrap())
        .filter(|event| event["event"] == "catalog_schema_migrated")
        .collect()
}

#[test]
fn schema_upgrade_is_logged_once_and_newer_schema_explains_recovery() {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    assert!(
        migration_events(&data).is_empty(),
        "fresh catalog is not a migration"
    );
    let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
    common::downgrade_object_schema(&connection);
    connection
        .execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; DROP TABLE bucket_policies; DROP TABLE policy_revision; DROP TABLE startup_seed; PRAGMA user_version = 4;")
        .unwrap();
    drop(connection);
    let events = migration_events(&data);
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["level"], "info");
    assert_eq!(events[0]["from"], 4);
    assert!(
        migration_events(&data).is_empty(),
        "migrated catalog reopened"
    );

    // Derive the current schema so later schema bumps do not need to edit this test.
    let path = data.join("catalog.sqlite3");
    let connection = rusqlite::Connection::open(&path).unwrap();
    let current: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(events[0]["to"], current);
    let newer = current + 1;
    connection
        .pragma_update(None, "user_version", newer)
        .unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();
    fails(
        &mut command(&data, None),
        &format!(
            "catalog schema version {newer} was written by a newer gcs-mimic release; this release reads schema {current} and older"
        ),
    );
    fails(
        &mut command(&data, None),
        "migrated catalogs cannot be downgraded",
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[tokio::test]
async fn probes_cannot_succeed_while_seed_publication_is_blocked() {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"fixture");
    let catalog = Catalog::open(&data).await.unwrap();
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut child = command(&data, Some(&seed))
        .args(["--listen", &address.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut server = Server {
        child,
        stdout,
        address: address.to_string(),
        ready: Value::Null,
        strict: false,
    };
    // The startup event confirms ownership/recovery without a timing assumption.
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["event"],
        "seeding"
    );
    assert!(
        Catalog::open(&data).await.is_err(),
        "child must own the catalog while waiting for seed publication"
    );
    for _ in 0..10 {
        assert!(server.child.try_wait().unwrap().is_none());
        assert!(TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_err());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    connection.execute_batch("COMMIT;").unwrap();
    let mut line = String::new();
    server.stdout.read_line(&mut line).unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["seed"]["objects"], 1);
    server.ready = ready;
    assert_eq!(server.request("GET", "/_internal/health", b"").0, 200);
    assert_eq!(server.request("GET", "/_internal/ready", b"").0, 200);
    server.stop();
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_during_seed_drains_writes_releases_lock_and_retries() {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"fixture");
    let catalog = Catalog::open(&data).await.unwrap();
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let mut child = command(&data, Some(&seed))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let events = startup_events(&mut child);
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut server = Server {
        child,
        stdout,
        address: String::new(),
        ready: Value::Null,
        strict: false,
    };
    let event = events
        .recv_timeout(Duration::from_secs(5))
        .expect("seed must start");
    assert_eq!(event["event"], "seeding");
    // "seeding" precedes the fixture scan; TERM before the first catalog write would test an
    // idle shutdown instead. The blocked write waits up to the 5-second busy timeout.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        Command::new("kill")
            .args(["-TERM", &server.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        events.recv_timeout(Duration::from_secs(3)).unwrap()["event"],
        "stopping"
    );
    // A pending SQLite mutation must keep ownership until it settles, even after TERM.
    assert!(Catalog::open(&data).await.is_err());
    assert!(server.child.try_wait().unwrap().is_none());
    connection.execute_batch("COMMIT;").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "seed ignored SIGTERM after owned write drained"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut output = String::new();
    server.stdout.read_to_string(&mut output).unwrap();
    assert_eq!(output, "{\"event\":\"stopped\"}\n");
    let reopened = Catalog::open(&data).await.unwrap();
    reopened.close().await.unwrap();
    let retried = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(
        retried.request("GET", "/storage/v1/b/local-bucket/o/file?alt=media", b""),
        (200, b"fixture".to_vec())
    );
    retried.stop();
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_during_seed_body_and_precommit_cleans_only_uncommitted_files() {
    use std::collections::BTreeSet;
    const SIZE: u64 = 64 * 1024 * 1024;
    for after_rename in [false, true] {
        let dir = TestDir::new();
        let data = dir.0.join("data");
        let seed = dir.0.join("fixtures");
        fixture(&seed, "local-bucket/large", b"");
        fs::OpenOptions::new()
            .write(true)
            .open(seed.join("local-bucket/large"))
            .unwrap()
            .set_len(SIZE)
            .unwrap();
        let mut child = command(&data, Some(&seed))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let events = startup_events(&mut child);
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Server {
            child,
            stdout,
            address: String::new(),
            ready: Value::Null,
            strict: false,
        };
        assert_eq!(
            events.recv_timeout(Duration::from_secs(5)).unwrap()["event"],
            "seeding"
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let transferring = fs::read_dir(data.join("staging"))
                .into_iter()
                .flatten()
                .any(|entry| {
                    let size =
                        fs::metadata(entry.unwrap().path()).map_or(0, |metadata| metadata.len());
                    // File publication may clone the complete source before hashing it.
                    size > 0 && size <= SIZE
                });
            if transferring {
                break;
            }
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "seed exited before body transfer"
            );
            assert!(
                tokio::time::Instant::now() < deadline,
                "seed did not reach body transfer"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
        let blob_ids = || -> BTreeSet<String> {
            fs::read_dir(data.join("blobs"))
                .unwrap()
                .flat_map(|shard| fs::read_dir(shard.unwrap().path()).unwrap())
                .map(|blob| blob.unwrap().file_name().into_string().unwrap())
                .collect()
        };
        if after_rename {
            // As in the existing media crash test, reserve SQLite after advisory checks and block publication after rename.
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while blob_ids().is_empty() {
                assert!(
                    server.child.try_wait().unwrap().is_none(),
                    "seed exited before publication"
                );
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "seed did not reach rename before commit"
                );
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM objects", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert!(
            Command::new("kill")
                .args(["-TERM", &server.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert_eq!(
            events.recv_timeout(Duration::from_secs(3)).unwrap()["event"],
            "stopping"
        );
        if after_rename {
            assert!(
                Catalog::open(&data).await.is_err(),
                "owned publication must retain the lock"
            );
            connection.execute_batch("COMMIT").unwrap();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = server.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "seed ignored SIGTERM during transfer/publication"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut output = String::new();
        server.stdout.read_to_string(&mut output).unwrap();
        assert_eq!(output, "{\"event\":\"stopped\"}\n");
        assert_eq!(fs::read_dir(data.join("staging")).unwrap().count(), 0);
        let referenced = connection
            .prepare("SELECT blob_id FROM objects")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<Result<BTreeSet<_>, _>>()
            .unwrap();
        assert_eq!(
            blob_ids(),
            referenced,
            "unreferenced blobs must be reclaimed before stopped"
        );
        if after_rename {
            assert_eq!(
                referenced.len(),
                1,
                "owned commit must finish before unlock"
            );
        } else {
            assert!(
                referenced.is_empty(),
                "partial body must not commit after cancellation"
            );
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM objects", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        drop(connection);
        let reopened = Catalog::open(&data).await.unwrap();
        reopened.close().await.unwrap();
        let retried = Server::start(&mut command(&data, Some(&seed)), false);
        assert_eq!(
            retried.json("/storage/v1/b/local-bucket/o/large")["size"],
            SIZE.to_string()
        );
        let mut stream = TcpStream::connect(&retried.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(b"GET /storage/v1/b/local-bucket/o/large?alt=media HTTP/1.1\r\nHost: localhost\r\nRange: bytes=0-15\r\nConnection: close\r\n\r\n").unwrap();
        let mut response = Vec::new();
        stream.take(65536).read_to_end(&mut response).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 206"));
        assert!(response.ends_with(&[0; 16]));
        retried.stop();
    }
}
