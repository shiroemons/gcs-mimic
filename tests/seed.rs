mod common;

use common::TestDir;
use gcs_mimic::{config::Config, object::ObjectConditions, storage::Catalog};
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
        .args(["serve", "--data-dir"])
        .arg(data);
    if let Some(seed) = seed {
        command.arg("--seed-dir").arg(seed);
    }
    command
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
        r#"{"seed_dir":"file-seed","seed_project":"file-project"}"#,
    )
    .unwrap();
    let mut args = vec!["--config".into(), file.to_str().unwrap().into()];
    let defaults = Config::load(&[], &BTreeMap::new()).unwrap();
    assert_eq!(defaults.seed_dir, None);
    assert_eq!(defaults.seed_project, "local-project");
    let loaded = Config::load(&args, &BTreeMap::new()).unwrap();
    assert_eq!(loaded.seed_dir.as_deref(), Some(Path::new("file-seed")));
    assert_eq!(loaded.seed_project, "file-project");
    let env = BTreeMap::from([
        ("GCS_MIMIC_SEED_DIR".into(), "env-seed".into()),
        ("GCS_MIMIC_SEED_PROJECT".into(), "env-project".into()),
    ]);
    let loaded = Config::load(&args, &env).unwrap();
    assert_eq!(loaded.seed_dir.as_deref(), Some(Path::new("env-seed")));
    assert_eq!(loaded.seed_project, "env-project");
    args.extend([
        "--seed-dir".into(),
        "cli-seed".into(),
        "--seed-project".into(),
        "cli-project".into(),
    ]);
    let loaded = Config::load(&args, &env).unwrap();
    assert_eq!(loaded.seed_dir.as_deref(), Some(Path::new("cli-seed")));
    assert_eq!(loaded.seed_project, "cli-project");
    for (key, invalid) in [
        ("seed_dir", ""),
        ("seed_project", ""),
        ("seed_project", "with space"),
        ("seed_project", "line\nfeed"),
        ("seed_project", &"x".repeat(257)),
    ] {
        let flag = format!("--{}", key.replace('_', "-"));
        assert!(Config::load(&[flag, invalid.into()], &BTreeMap::new()).is_err());
        let env = BTreeMap::from([(
            format!("GCS_MIMIC_{}", key.to_ascii_uppercase()),
            invalid.into(),
        )]);
        assert!(Config::load(&[], &env).is_err());
        fs::write(&file, json!({key:invalid}).to_string()).unwrap();
        assert!(Config::load(&args[..2], &BTreeMap::new()).is_err());
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
            json!({"outcome":"imported","buckets":2,"objects":5,"existing_objects":0})
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
        assert_eq!(metadata["size"], "5");
        assert_eq!(metadata["md5Hash"], "XUFAKrxLKna5cZ2REBfFkg==");
        assert_eq!(metadata["crc32c"], "mnG7TA==");
        assert_eq!(metadata["contentType"], "application/octet-stream");
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
fn restart_preserves_mutations_and_deleted_buckets_and_new_data_reseeds() {
    let dir = TestDir::new();
    let seed = dir.0.join("fixtures");
    let data = dir.0.join("data");
    fixture(&seed, "local-bucket/hello.txt", b"fixture");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/local-bucket/o?uploadType=media&name=hello.txt",
                b"mutation"
            )
            .0,
        200
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/local-bucket/o?uploadType=media&name=created.txt",
                b"new"
            )
            .0,
        200
    );
    assert_eq!(
        server
            .request(
                "PATCH",
                "/storage/v1/b/local-bucket/o/hello.txt",
                br#"{"metadata":{"owner":"user"}}"#
            )
            .0,
        200
    );
    server.stop();
    fs::remove_dir_all(&seed).unwrap();
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"]["outcome"], "skipped");
    assert_eq!(
        server.request(
            "GET",
            "/storage/v1/b/local-bucket/o/hello.txt?alt=media",
            b""
        ),
        (200, b"mutation".to_vec())
    );
    assert_eq!(
        server.json("/storage/v1/b/local-bucket/o/hello.txt")["metadata"]["owner"],
        "user"
    );
    assert_eq!(
        server.request(
            "GET",
            "/storage/v1/b/local-bucket/o/created.txt?alt=media",
            b""
        ),
        (200, b"new".to_vec())
    );
    for object in ["hello.txt", "created.txt"] {
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("/storage/v1/b/local-bucket/o/{object}"),
                    b""
                )
                .0,
            204
        );
    }
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/local-bucket", b"")
            .0,
        204
    );
    server.stop();
    fixture(&seed, "local-bucket/hello.txt", b"fixture");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(
        server.request("GET", "/storage/v1/b/local-bucket", b"").0,
        404
    );
    server.stop();
    let server = Server::start(&mut command(&dir.0.join("fresh-data"), Some(&seed)), false);
    assert_eq!(
        server.request(
            "GET",
            "/storage/v1/b/local-bucket/o/hello.txt?alt=media",
            b""
        ),
        (200, b"fixture".to_vec())
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
        .get_object("local-bucket", "a-valid", None, ObjectConditions::default())
        .await
        .unwrap();
    catalog.close().await.unwrap();
    fails(
        command(&data, Some(&seed)).args(["--seed-project", "other-project"]),
        "belongs to another project",
    );
    fs::remove_dir_all(seed.join("z-Invalid")).unwrap();
    fixture(&seed, "local-bucket/a-valid", b"changed fixture");
    fixture(&seed, "local-bucket/z-valid", b"new");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"]["existing_objects"], 1);
    assert_eq!(server.ready["seed"]["objects"], 1);
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
        ("top.txt", "top-level entries"),
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
    symlink(
        seed.join("local-bucket/file"),
        seed.join("local-bucket/link"),
    )
    .unwrap();
    fails(
        &mut command(&dir.0.join("link-data"), Some(&seed)),
        "only regular files",
    );
    fs::remove_file(seed.join("local-bucket/link")).unwrap();
    let root_link = dir.0.join("root-link");
    symlink(&seed, &root_link).unwrap();
    fails(
        &mut command(&dir.0.join("root-link-data"), Some(&root_link)),
        "not a symlink",
    );
    let socket = seed.join("local-bucket/socket");
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
fn disabled_startup_never_replays_fixtures_later() {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    let server = Server::start(&mut command(&data, None), false);
    assert_eq!(server.ready["seed"]["outcome"], "disabled");
    server.stop();
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"fixture");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"]["outcome"], "skipped");
    assert_eq!(
        server.request("GET", "/storage/v1/b/local-bucket", b"").0,
        404
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
        json!({"outcome":"imported", "buckets":0, "objects":0, "existing_objects":0})
    );
    server.stop();
}

#[tokio::test]
async fn populated_v4_catalog_migrates_without_seeding() {
    let dir = TestDir::new();
    let data = dir.0.join("data");
    let seed = dir.0.join("fixtures");
    fixture(&seed, "local-bucket/file", b"fixture");
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/local-bucket/o?uploadType=media&name=file",
                b"user"
            )
            .0,
        200
    );
    server.stop();
    let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch("DROP TABLE bucket_policies; DROP TABLE policy_revision; DROP TABLE startup_seed; PRAGMA user_version = 4;")
        .unwrap();
    drop(connection);
    let server = Server::start(&mut command(&data, Some(&seed)), false);
    assert_eq!(server.ready["seed"]["outcome"], "skipped");
    assert_eq!(
        server.request("GET", "/storage/v1/b/local-bucket/o/file?alt=media", b""),
        (200, b"user".to_vec())
    );
    server.stop();
    let catalog = Catalog::open(&data).await.unwrap();
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(data.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
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
