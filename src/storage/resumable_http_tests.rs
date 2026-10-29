use super::catalog::replace_resumable_cleanup_obstacle;
#[path = "object_retention_http_tests.rs"]
mod object_retention;
#[path = "retention_http_tests.rs"]
mod retention;
#[path = "soft_delete_http_tests.rs"]
mod soft_delete;
use crate as gcs_mimic;
use crate::test_support::TestDir;
use gcs_mimic::storage::admin::{BucketDefinition, BucketName, ObjectName, Origin};
use gcs_mimic::{
    auth::{Auth, Registry},
    bucket::BucketRecord,
    clock::Clock,
    config::Config,
    server::{State, serve},
    storage::Catalog,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};

struct Harness {
    address: String,
    base: String,
    state: Arc<State>,
    clock: Arc<TestClock>,
    stop: oneshot::Sender<()>,
    server: JoinHandle<std::io::Result<()>>,
    // Fields drop in order; the directory must outlive the catalog in `state`,
    // because Windows cannot delete files the catalog still holds.
    dir: TestDir,
}
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
struct TestClock(Mutex<SystemTime>);
impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }
}
impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
    fn error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        assert_eq!(self.json()["error"]["errors"][0]["reason"], reason);
    }
}
impl Harness {
    async fn start(max: u64, strict: bool, base: Option<&str>) -> Self {
        Self::start_with_catalog_wait(max, strict, base, 30000).await
    }
    async fn start_with_catalog_wait(
        max: u64,
        strict: bool,
        base: Option<&str>,
        catalog_wait_timeout_ms: u64,
    ) -> Self {
        let dir = TestDir::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let base = base
            .map(str::to_owned)
            .unwrap_or_else(|| format!("http://{address}"));
        let clock = Arc::new(TestClock(Mutex::new(SystemTime::now())));
        let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
            .await
            .unwrap();
        catalog
            .admin(Origin::Seed)
            .ensure_bucket(
                BucketDefinition::new(BucketRecord {
                    name: "bucket".into(),
                    project: "project".into(),
                    location: "US".into(),
                    location_type: "multi-region".into(),
                    storage_class: "STANDARD".into(),
                    labels: Default::default(),
                    versioning_enabled: false,
                    default_event_based_hold: false,
                    public_access_prevention_enforced: false,
                    uniform_bucket_level_access_locked_time: None,
                    lifecycle: Vec::new(),
                    retention_policy: None,
                    soft_delete_policy: None,
                    object_retention_enabled: false,
                    cors: Vec::new(),
                    metageneration: 1,
                    time_created: UNIX_EPOCH,
                    updated: UNIX_EPOCH,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let auth = if strict {
            Auth::strict(Registry::parse(json!({"identities":[{"id":"admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},{"id":"creator@example.test","tokens":["creator"],"project_roles":{"project":["roles/storage.objectCreator"]}},{"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"project":["roles/storage.objectViewer"]}}]}).to_string().as_bytes()).unwrap())
        } else {
            Auth::permissive()
        };
        let state = Arc::new(State {
            ready: AtomicBool::new(true),
            clock: clock.clone(),
            catalog,
            api_base_url: base.trim_end_matches('/').into(),
            default_location: "US".into(),
            auth,
        });
        let config = Config {
            scheme: gcs_mimic::config::Scheme::Http,
            api_base_url: Some(state.api_base_url.clone()),
            max_object_bytes: max,
            catalog_wait_timeout_ms,
            ..Default::default()
        };
        let (stop, rx) = oneshot::channel();
        let running = state.clone();
        let server = tokio::spawn(async move {
            serve(listener, config, running, async {
                rx.await.unwrap();
                Ok(())
            })
            .await
        });
        Self {
            dir,
            address,
            base,
            state,
            clock,
            stop,
            server,
        }
    }
    async fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let (mut reader, mut writer) = TcpStream::connect(&self.address)
            .await
            .unwrap()
            .into_split();
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: spoofed.invalid\r\nForwarded: host=evil.invalid;proto=https\r\nX-Forwarded-Host: evil.invalid\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        );
        // The server may answer before reading the whole body, for example with a
        // 503 when a short catalog budget expires, and then close the connection.
        // Reading while writing keeps that response for the caller to retry
        // instead of failing the remaining write with a broken pipe.
        let write = async {
            writer.write_all(head.as_bytes()).await?;
            writer.write_all(body).await
        };
        let read = async {
            let mut raw = Vec::new();
            let result = Self::read_framed(&mut BufReader::new(&mut reader), &mut raw).await;
            (raw, result)
        };
        let (written, (raw, read)) = tokio::join!(write, read);
        for (stage, result) in [("write", written), ("read", read)] {
            if let Err(error) = result {
                let closed = matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::UnexpectedEof
                );
                assert!(
                    closed && !raw.is_empty(),
                    "{method} {path}: {stage} failed without a response: {error}"
                );
            }
        }
        Self::parse_response(&raw)
    }
    async fn read_response(stream: TcpStream) -> Reply {
        let mut raw = Vec::new();
        Self::read_framed(&mut BufReader::new(stream), &mut raw)
            .await
            .unwrap();
        Self::parse_response(&raw)
    }
    /// Reads one response, after any 1xx interim responses, framed by its status,
    /// Content-Length or chunked encoding, because the server keeps the connection
    /// open for further requests instead of closing it after the response.
    async fn read_framed(
        reader: &mut (impl AsyncBufRead + Unpin),
        raw: &mut Vec<u8>,
    ) -> std::io::Result<()> {
        async fn line(
            reader: &mut (impl AsyncBufRead + Unpin),
            raw: &mut Vec<u8>,
        ) -> std::io::Result<usize> {
            let start = raw.len();
            if reader.read_until(b'\n', raw).await? == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            Ok(start)
        }
        let head = loop {
            let start = raw.len();
            while !raw[start..].ends_with(b"\r\n\r\n") {
                line(reader, raw).await?;
            }
            let head = String::from_utf8_lossy(&raw[start..]).to_ascii_lowercase();
            let status: u16 = head
                .split_whitespace()
                .nth(1)
                .and_then(|status| status.parse().ok())
                .ok_or(std::io::ErrorKind::InvalidData)?;
            if !(100..200).contains(&status) {
                if status == 204 || status == 304 {
                    return Ok(());
                }
                break head;
            }
        };
        let header = |name: &str| {
            head.lines()
                .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
                .map(str::trim)
        };
        if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
            loop {
                let start = line(reader, raw).await?;
                let size = String::from_utf8_lossy(&raw[start..]).trim().to_owned();
                let size = usize::from_str_radix(size.split(';').next().unwrap_or(""), 16)
                    .map_err(|_| std::io::ErrorKind::InvalidData)?;
                if size == 0 {
                    while line(reader, raw)
                        .await
                        .map(|start| raw[start..] != *b"\r\n")?
                    {}
                    return Ok(());
                }
                let start = raw.len();
                raw.resize(start + size + 2, 0);
                reader.read_exact(&mut raw[start..]).await?;
            }
        }
        let length: usize = header("content-length")
            .ok_or(std::io::ErrorKind::InvalidData)?
            .parse()
            .map_err(|_| std::io::ErrorKind::InvalidData)?;
        let start = raw.len();
        raw.resize(start + length, 0);
        reader.read_exact(&mut raw[start..]).await?;
        Ok(())
    }
    fn parse_response(raw: &[u8]) -> Reply {
        let split = raw.windows(4).position(|v| v == b"\r\n\r\n").unwrap();
        let header = std::str::from_utf8(&raw[..split]).unwrap();
        let mut lines = header.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: BTreeMap<String, String> = lines
            .map(|line| {
                let (k, v) = line.split_once(':').unwrap();
                (k.to_ascii_lowercase(), v.trim().to_owned())
            })
            .collect();
        let mut body = raw[split + 4..].to_vec();
        if headers
            .get("transfer-encoding")
            .is_some_and(|v| v == "chunked")
        {
            let mut rest = body.as_slice();
            let mut decoded = Vec::new();
            loop {
                let end = rest.windows(2).position(|v| v == b"\r\n").unwrap();
                let n =
                    usize::from_str_radix(std::str::from_utf8(&rest[..end]).unwrap(), 16).unwrap();
                if n == 0 {
                    break;
                }
                rest = &rest[end + 2..];
                decoded.extend_from_slice(&rest[..n]);
                rest = &rest[n + 2..];
            }
            body = decoded;
        }
        Reply {
            status,
            headers,
            body,
        }
    }
    async fn initiate(&self, query: &str, headers: &str, body: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/bucket/o?uploadType=resumable&{query}"),
            headers,
            body,
        )
        .await
    }
    fn path(&self, reply: &Reply) -> String {
        reply.headers["location"]
            .strip_prefix(self.base.trim_end_matches('/'))
            .unwrap()
            .to_owned()
    }
    async fn stop(self) {
        self.stop.send(()).unwrap();
        self.server.await.unwrap().unwrap();
    }
}

/// Retries a request that a short catalog budget may reject with 503 while the
/// runner is loaded; a persistent 503 still fails after five seconds.
async fn retry_unavailable<F: std::future::Future<Output = Reply>>(
    mut request: impl FnMut() -> F,
) -> Reply {
    // A hang guard: a loaded runner may need many retries before the catalog frees.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let reply = request().await;
        if reply.status != 503 || tokio::time::Instant::now() >= deadline {
            return reply;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn assert_catalog_wait_timeout(reply: &Reply) {
    assert_eq!(reply.status, 503);
    assert!(
        reply.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("upload catalog wait timed out")
    );
}

#[tokio::test]
async fn obj_http_resumable_independent_empty_and_multichunk_checksum_vectors() {
    let h = Harness::start(1024 * 1024, false, None).await;
    // Expected values use OpenSSL MD5 and an independent bitwise Castagnoli reference.
    for (size, md5, crc32c) in [
        (0, "1B2M2Y8AsgTpgAmY7PhCfg==", "AAAAAA=="),
        (262145, "rXaC3/oyxEygvmW1+8ETpQ==", "srP9bA=="),
    ] {
        for supplied in [false, true] {
            let metadata = if supplied {
                json!({"md5Hash":md5,"crc32c":crc32c})
            } else {
                json!({})
            }
            .to_string();
            let init = h
                .initiate(
                    "name=vector",
                    "Content-Type: application/json\r\n",
                    metadata.as_bytes(),
                )
                .await;
            assert_eq!(init.status, 200);
            let path = h.path(&init);
            let mut start = 0;
            if size > 0 {
                assert_eq!(
                    h.request(
                        "PUT",
                        &path,
                        &format!("Content-Range: bytes 0-262143/{size}\r\n"),
                        &vec![0; 262144]
                    )
                    .await
                    .status,
                    308
                );
                start = 262144;
            }
            let headers = if size == 0 {
                "Content-Range: bytes */0\r\n".to_owned()
            } else {
                format!("Content-Range: bytes {start}-{}/{size}\r\n", size - 1)
            };
            let reply = h
                .request("PUT", &path, &headers, &vec![0; size - start])
                .await;
            assert_eq!(reply.status, 200);
            assert_eq!(reply.json()["md5Hash"], md5);
            assert_eq!(reply.json()["crc32c"], crc32c);
            assert_eq!(
                h.request("GET", "/storage/v1/b/bucket/o/vector?alt=media", "", b"")
                    .await
                    .body,
                vec![0; size]
            );
            assert_eq!(
                std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
                0
            );
        }
    }
    h.stop().await;
}

#[tokio::test]
async fn obj_http_resumable_failed_overwrites_preserve_metadata_bytes_and_files() {
    let h = Harness::start(32, false, None).await;
    h.request(
        "POST",
        "/upload/storage/v1/b/bucket/o?uploadType=media&name=object",
        "",
        b"old",
    )
    .await;
    let original = h
        .request(
            "PATCH",
            "/storage/v1/b/bucket/o/object",
            "Content-Type: application/json\r\n",
            br#"{"contentType":"text/old","metadata":{"key":"old"}}"#,
        )
        .await
        .json();
    for metadata in [
        br#"{"md5Hash":"AAAAAAAAAAAAAAAAAAAAAA=="}"#.as_slice(),
        br#"{"crc32c":"AAAAAA=="}"#,
    ] {
        let init = h
            .initiate(
                "name=object",
                "Content-Type: application/json\r\n",
                metadata,
            )
            .await;
        assert_eq!(init.status, 200);
        h.request("PUT", &h.path(&init), "", b"new")
            .await
            .error(400, "invalid");
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
                .await
                .json(),
            original
        );
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
                .await
                .body,
            b"old"
        );
        assert_eq!(
            std::fs::read_dir(h.dir.0.join("blobs"))
                .unwrap()
                .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
            0
        );
    }
    let init = h.initiate("name=object", "", b"").await;
    let path = h.path(&init);
    h.request("PUT", &path, "", &[b'x'; 33])
        .await
        .error(413, "uploadTooLarge");
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .json(),
        original
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
            .await
            .body,
        b"old"
    );
    // Header-only size rejection leaves an untouched session until cancellation.
    let status = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(status.status, 308);
    assert!(!status.headers.contains_key("range"));
    assert_eq!(h.request("DELETE", &path, "", b"").await.status, 499);
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        0
    );
    let init = h.initiate("name=object", "", b"").await;
    let path = h.path(&init);
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: local\r\nContent-Range: bytes 0-2/3\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nnew\r\ninvalid\r\n").as_bytes()).await.unwrap();
    Harness::read_response(stream)
        .await
        .error(400, "badRequest");
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .json(),
        original
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
            .await
            .body,
        b"old"
    );
    // A body error leaves a retryable session; cancellation must remove its owned staging file.
    assert_eq!(h.request("DELETE", &path, "", b"").await.status, 499);
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .json(),
        original
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
            .await
            .body,
        b"old"
    );
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("blobs"))
            .unwrap()
            .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum::<usize>(),
        1
    );
    h.stop().await;
}

#[tokio::test]
async fn initiation_returns_opaque_fixed_location_and_status_without_publication() {
    let h = Harness::start(1000, false, Some("https://configured.invalid/proxy///")).await;
    let reply = h
        .initiate("name=hello", "Origin: https://client.invalid\r\n", b"")
        .await;
    assert_eq!(reply.status, 200);
    assert!(reply.body.is_empty());
    assert_eq!(reply.headers["cache-control"], "no-store");
    assert!(
        reply.headers["location"]
            .starts_with("https://configured.invalid/proxy/upload/storage/v1/")
    );
    let path = reply.headers["location"]
        .strip_prefix("https://configured.invalid/proxy")
        .unwrap();
    for range in ["bytes */*", "bytes */5"] {
        let r = h
            .request("PUT", path, &format!("Content-Range: {range}\r\n"), b"")
            .await;
        assert_eq!(r.status, 308);
        assert!(r.body.is_empty());
        assert!(!r.headers.contains_key("range"));
        assert!(!r.headers.contains_key("location"));
    }
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/hello", "", b"")
            .await
            .status,
        404
    );
    h.stop().await;
}

#[tokio::test]
async fn initiation_preserves_encoded_names_metadata_and_query_precedence() {
    let h = Harness::start(1000, false, None).await;
    let reply=h.initiate("name=%E6%97%A5%2F%25&contentEncoding=gzip&projection=full","Content-Type: application/json\r\nX-Upload-Content-Type: image/png\r\nX-Upload-Content-Length: 0\r\n",br#"{"name":"ignored","bucket":null,"contentType":"application/test","metadata":{"key":"value"}}"#).await;
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let id = reply.headers["location"]
        .split("upload_id=")
        .nth(1)
        .unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    assert_eq!(session.name, "日/%");
    assert_eq!(session.options.content_type, "application/test");
    assert_eq!(
        session.options.metadata.content_encoding,
        Some(Some("gzip".into()))
    );
    assert_eq!(session.total, Some(0));
    let r = h.request("PUT", &h.path(&reply), "", b"").await;
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["name"], "日/%");
    assert_eq!(r.json()["contentType"], "application/test");
    assert_eq!(r.json()["contentEncoding"], "gzip");
    assert_eq!(r.json()["metadata"]["key"], "value");
    h.stop().await;
}

#[tokio::test]
async fn invalid_initiation_and_status_leave_no_session_or_file() {
    let h = Harness::start(10, false, None).await;
    for (query, headers, body, status) in [
        (
            "name=a",
            "X-Upload-Content-Length: 11\r\n",
            b"".as_slice(),
            413,
        ),
        ("name=a", "X-Upload-Content-Length: -1\r\n", b"", 400),
        (
            "name=a",
            "X-Upload-Content-Length: 18446744073709551616\r\n",
            b"",
            400,
        ),
        (
            "name=a",
            "X-Upload-Content-Length: 1\r\nX-Upload-Content-Length: 2\r\n",
            b"",
            400,
        ),
        ("name=a&name=b", "", b"", 400),
        ("name=a&ifGenerationMatch=-1", "", b"", 400),
        (
            "name=a",
            "Content-Type: application/json\r\n",
            br#"{"name":"a","name":"b"}"#,
            400,
        ),
        ("name=a", "Content-Type: text/plain\r\n", b"{}", 400),
        ("name=a", "X-Goog-Upload-Command: start\r\n", b"", 400),
        ("name=a", "X-Goog-Gcs-Idempotency-Token: \r\n", b"", 400),
        (
            "name=a",
            "X-Goog-Gcs-Idempotency-Token: first\r\nX-Goog-Gcs-Idempotency-Token: second\r\n",
            b"",
            400,
        ),
        (
            "name=a&contentEncoding=gzip",
            "Content-Type: application/json\r\n",
            br#"{"contentEncoding":"identity"}"#,
            400,
        ),
    ] {
        assert_eq!(
            h.initiate(query, headers, body).await.status,
            status,
            "{headers}"
        );
    }
    assert_eq!(
        h.initiate(
            "name=a",
            "Content-Type: application/json\r\n",
            &vec![b' '; 65537]
        )
        .await
        .status,
        413
    );
    let metadata = json!({"metadata":{"key":"x".repeat(8192)}}).to_string();
    assert_eq!(
        h.initiate(
            "name=a",
            "Content-Type: application/json\r\n",
            metadata.as_bytes()
        )
        .await
        .status,
        400
    );
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        0
    );
    let initiated = h
        .initiate("name=good", "X-Upload-Content-Length: 8\r\n", b"")
        .await;
    let path = h.path(&initiated);
    for (range, body) in [
        ("bytes */7", b"".as_slice()),
        ("bytes */*", b"x"),
        ("bytes */-1", b""),
    ] {
        assert_eq!(
            h.request("PUT", &path, &format!("Content-Range: {range}\r\n"), body)
                .await
                .status,
            400
        );
    }
    let invalid = h
        .request(
            "PUT",
            "/upload/storage/v1/b/bucket/o?upload_id=unknown",
            "Content-Range: bytes */*\r\n",
            b"",
        )
        .await;
    assert_eq!(invalid.status, 404);
    assert_eq!(invalid.json()["error"]["errors"][0]["reason"], "notFound");
    h.stop().await;
}

#[tokio::test]
async fn initiation_checks_strict_create_and_overwrite_permissions_before_staging() {
    let h = Harness::start(1000, true, None).await;
    for (token, status) in [("", 401), ("invalid", 401), ("viewer", 403)] {
        let headers = if token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {token}\r\n")
        };
        assert_eq!(
            h.initiate("name=object", &headers, b"").await.status,
            status
        );
    }
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        0
    );
    assert_eq!(
        h.initiate("name=new", "Authorization: Bearer creator\r\n", b"")
            .await
            .status,
        200
    );
    assert_eq!(
        h.request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=media&name=old",
            "Authorization: Bearer admin\r\n",
            b"old"
        )
        .await
        .status,
        200
    );
    assert_eq!(
        h.initiate("name=old", "Authorization: Bearer creator\r\n", b"")
            .await
            .status,
        403
    );
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        1
    );
    h.stop().await;
}

#[tokio::test]
async fn initiation_session_cap_and_status_prefix_are_bounded() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let reply = h.initiate("name=object", "", b"").await;
    let path = h.path(&reply);
    let id = reply.headers["location"]
        .split("upload_id=")
        .nth(1)
        .unwrap();
    let mut lease = h
        .state
        .catalog
        .lock_resumable_session(id, "bucket")
        .await
        .unwrap();
    std::fs::write(lease.staging_path(), b"prefix-extra-tail").unwrap();
    h.state
        .catalog
        .checkpoint_resumable_session(&mut lease, 6, None)
        .await
        .unwrap();
    drop(lease);
    for _ in 0..2 {
        let r = h
            .request(
                "POST",
                &path,
                "Content-Range: bytes */*\r\nX-GUploader-No-308: yes\r\n",
                b"",
            )
            .await;
        assert_eq!(r.status, 200);
        assert_eq!(r.headers["range"], "bytes=0-5");
        assert_eq!(r.headers["x-http-status-code-override"], "308");
        assert!(r.body.is_empty());
    }
    for _ in 1..128 {
        assert_eq!(h.initiate("name=object", "", b"").await.status, 200);
    }
    assert_eq!(h.initiate("name=rejected", "", b"").await.status, 503);
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        128
    );
    h.stop().await;
}

#[tokio::test]
async fn sequential_binary_chunks_publish_exact_bytes_hashes_and_metadata_only_at_completion() {
    use md5::{Digest, Md5};
    let h = Harness::start(1024 * 1024, false, None).await;
    let metadata=br#"{"contentType":"application/test","contentDisposition":"inline","metadata":{"first":"kept","replace":"old"}}"#;
    let init = h
        .initiate(
            "name=object",
            "Content-Type: application/json\r\n",
            metadata,
        )
        .await;
    let path = h.path(&init);
    let first: Vec<u8> = (0..256 * 1024).map(|n| (n % 251) as u8).collect();
    let final_bytes = b"last\0\xff\r\n";
    let total = first.len() + final_bytes.len();
    let before = h.clock.now();
    let r = h
        .request(
            "PUT",
            &path,
            &format!("Content-Range: bytes 0-{}/{total}\r\n", first.len() - 1),
            &first,
        )
        .await;
    assert_eq!(r.status, 308, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.headers["range"], format!("bytes=0-{}", first.len() - 1));
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .status,
        404
    );
    assert!(
        h.request("GET", "/storage/v1/b/bucket/o", "", b"")
            .await
            .json()
            .get("items")
            .is_none()
    );
    *h.clock.0.lock().unwrap() += std::time::Duration::from_secs(2);
    let r=h.request("PUT",&path,&format!("Content-Range: bytes {}-{}/{total}\r\nX-Goog-Meta-Replace: new\r\nX-Goog-Meta-Added: final\r\n",first.len(),total-1),final_bytes).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    let resource = r.json();
    assert_eq!(resource["size"], total.to_string());
    assert_eq!(resource["metageneration"], "1");
    assert_eq!(resource["contentType"], "application/test");
    assert_eq!(resource["contentDisposition"], "inline");
    assert_eq!(
        resource["metadata"],
        json!({"first":"kept","replace":"new","added":"final"})
    );
    let mut expected = first;
    expected.extend_from_slice(final_bytes);
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
            .await
            .body,
        expected
    );
    let record = h
        .state
        .catalog
        .admin(Origin::Seed)
        .get_object(
            &BucketName::new("bucket").unwrap(),
            &ObjectName::new("object").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(record.md5, <[u8; 16]>::from(Md5::digest(&expected)));
    assert_eq!(record.crc32c, crc32c::crc32c(&expected));
    assert_eq!(
        record
            .time_created
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
        h.clock
            .now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
    );
    assert!(record.time_created >= before);
    h.stop().await;
}

#[tokio::test]
async fn unknown_total_sdk_post_duplicate_overlap_and_empty_final_preserve_prefix() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let init = h.initiate("name=object", "", b"").await;
    let path = h.path(&init);
    let chunk = vec![b'a'; 256 * 1024];
    let headers = "Content-Range: bytes 0-262143/*\r\nX-GUploader-No-308: yes\r\nX-Goog-Gcs-Idempotency-Token: per-chunk\r\n";
    let r = h.request("POST", &path, headers, &chunk).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.headers["x-http-status-code-override"], "308");
    assert_eq!(r.headers["range"], "bytes=0-262143");
    let r = h
        .request("POST", &path, headers, &vec![b'b'; 256 * 1024])
        .await;
    assert_eq!(r.headers["range"], "bytes=0-262143");
    let mut overlapping = vec![b'c'; 128 * 1024];
    overlapping.extend(vec![b'd'; 128 * 1024]);
    let r = h
        .request(
            "PUT",
            &path,
            "Content-Range: bytes 131072-393215/*\r\n",
            &overlapping,
        )
        .await;
    assert_eq!(r.status, 308);
    assert_eq!(r.headers["range"], "bytes=0-393215");
    let r = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(r.status, 308);
    let r = h
        .request(
            "POST",
            &path,
            "Content-Range: bytes */393216\r\nX-GUploader-No-308: yes\r\n",
            b"",
        )
        .await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert!(!r.headers.contains_key("x-http-status-code-override"));
    let mut expected = chunk;
    expected.extend(vec![b'd'; 128 * 1024]);
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
            .await
            .body,
        expected
    );
    h.stop().await;
}

#[tokio::test]
async fn single_empty_and_exact_multiple_uploads_replay_original_completion_after_overwrite() {
    let h = Harness::start(1024 * 1024, false, None).await;
    for (name, body, range) in [
        ("single", b"hello".as_slice(), ""),
        ("empty", b"", "Content-Range: bytes */0\r\n"),
        ("empty-no-range", b"", ""),
    ] {
        let init = h.initiate(&format!("name={name}"), "", b"").await;
        let path = h.path(&init);
        let r = h.request("PUT", &path, range, body).await;
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let snapshot = r.json();
        assert_eq!(h.request("PUT", &path, range, body).await.json(), snapshot);
        assert_eq!(
            h.request(
                "POST",
                &format!("/upload/storage/v1/b/bucket/o?uploadType=media&name={name}"),
                "",
                b"later"
            )
            .await
            .status,
            200
        );
        assert_eq!(
            h.request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
                .await
                .json(),
            snapshot
        );
        assert_eq!(h.request("PUT", &path, range, body).await.json(), snapshot);
    }
    let init = h.initiate("name=multiple", "", b"").await;
    let path = h.path(&init);
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes 0-262143/*\r\n",
            &vec![0; 256 * 1024]
        )
        .await
        .status,
        308
    );
    assert_eq!(
        h.request("POST", &path, "Content-Range: bytes */262144\r\n", b"")
            .await
            .status,
        200
    );
    h.stop().await;
}

#[tokio::test]
async fn malformed_ranges_and_wrong_lengths_never_advance_the_durable_prefix() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let init = h
        .initiate("name=object", "X-Upload-Content-Length: 524288\r\n", b"")
        .await;
    let path = h.path(&init);
    for headers in [
        "Content-Range: bytes 1-262144/524288\r\n",
        "Content-Range: bytes 9-1/524288\r\n",
        "Content-Range: bytes 0-18446744073709551615/*\r\n",
        "Content-Range: bytes 0-262143/524287\r\n",
        "Content-Range: bytes 0-262143/524288\r\nContent-Range: bytes 0-262143/524288\r\n",
        "Content-Range: bytes 0-2/524288\r\n",
    ] {
        assert_eq!(
            h.request("PUT", &path, headers, b"abc").await.status,
            400,
            "{headers}"
        );
    }
    let short = h
        .request(
            "PUT",
            &path,
            "Content-Range: bytes 0-262143/524288\r\n",
            b"short",
        )
        .await;
    assert_eq!(short.status, 400);
    let status = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(status.status, 308);
    assert!(!status.headers.contains_key("range"));
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes 0-262143/524288\r\n",
            &vec![b'x'; 256 * 1024]
        )
        .await
        .status,
        308
    );
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes 262144-524287/524288\r\n",
            &vec![b'y'; 256 * 1024]
        )
        .await
        .status,
        200
    );
    let bytes = h
        .request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
        .await
        .body;
    assert_eq!(&bytes[..256 * 1024], &vec![b'x'; 256 * 1024]);
    assert_eq!(&bytes[256 * 1024..], &vec![b'y'; 256 * 1024]);
    h.stop().await;
}

#[tokio::test]
async fn object_caps_apply_to_known_unknown_and_streamed_lengths() {
    let h = Harness::start(256 * 1024, false, None).await;
    let init = h.initiate("name=exact", "", b"").await;
    let path = h.path(&init);
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes 0-262143/262144\r\n",
            &vec![b'x'; 256 * 1024]
        )
        .await
        .status,
        200
    );
    for header in [
        "Content-Range: bytes 0-262144/*\r\n",
        "Content-Range: bytes 0-262144/262145\r\n",
        "Content-Range: bytes */262145\r\n",
    ] {
        let init = h.initiate("name=large", "", b"").await;
        let path = h.path(&init);
        assert_eq!(h.request("PUT", &path, header, b"").await.status, 413);
    }
    let init = h.initiate("name=large", "", b"").await;
    let path = h.path(&init);
    // Avoid racing the bounded unread-body drain when the declared length already rejects.
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream
        .write_all(
            format!("PUT {path} HTTP/1.1\r\nHost: spoofed.invalid\r\nContent-Length: 262145\r\nExpect: 100-continue\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(Harness::read_response(stream).await.status, 413);
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/large", "", b"")
            .await
            .status,
        404
    );
    h.stop().await;
}

#[tokio::test]
async fn checksum_and_final_metadata_failures_preserve_the_previous_full_resource() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let original = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=media&name=object",
            "",
            b"old",
        )
        .await
        .json();
    for (metadata, headers) in [
        (
            br#"{"md5Hash":"XUFAKrxLKna5cZ2REBfFkg=="}"#.as_slice(),
            "X-Goog-Hash: md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n",
        ),
        (b"{}", "X-Goog-Hash: md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n"),
        (b"{}", "X-Goog-Hash: crc32c=AAAAAA==\r\n"),
        (b"{}", "X-Goog-Hash: crc32c=malformed\r\n"),
        (
            b"{}",
            "X-Goog-Hash: md5=XUFAKrxLKna5cZ2REBfFkg==,md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n",
        ),
    ] {
        let init = h
            .initiate(
                "name=object",
                "Content-Type: application/json\r\n",
                metadata,
            )
            .await;
        let path = h.path(&init);
        let r = h.request("PUT", &path, headers, b"hello").await;
        assert_eq!(r.status, 400, "{headers}");
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
                .await
                .json(),
            original
        );
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
                .await
                .body,
            b"old"
        );
        assert_eq!(
            h.request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
                .await
                .status,
            410
        );
    }
    let init = h
        .initiate(
            "name=good",
            "Content-Type: application/json\r\n",
            br#"{"md5Hash":"XUFAKrxLKna5cZ2REBfFkg=="}"#,
        )
        .await;
    let path = h.path(&init);
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "X-Goog-Hash: md5=XUFAKrxLKna5cZ2REBfFkg==,crc32c=mnG7TA==,md5=XUFAKrxLKna5cZ2REBfFkg==\r\n",
            b"hello"
        )
        .await
        .status,
        200
    );
    let metadata = json!({"metadata":{"large":"x".repeat(8180)}}).to_string();
    let init = h
        .initiate(
            "name=object",
            "Content-Type: application/json\r\n",
            metadata.as_bytes(),
        )
        .await;
    let path = h.path(&init);
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "X-Goog-Meta-Extra: too-many-bytes\r\n",
            b"hello"
        )
        .await
        .status,
        400
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .json(),
        original
    );
    h.stop().await;
}

#[tokio::test]
async fn all_four_initiation_conditions_are_rechecked_atomically_and_cannot_be_relaxed() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let original = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=media&name=object",
            "",
            b"old",
        )
        .await
        .json();
    let generation = original["generation"].as_str().unwrap();
    let query = format!(
        "name=object&ifGenerationMatch={generation}&ifGenerationNotMatch=0&ifMetagenerationMatch=1&ifMetagenerationNotMatch=2"
    );
    let init = h.initiate(&query, "", b"").await;
    let path = h.path(&init);
    assert_eq!(
        h.request("PUT", &path, "", b"all four pass").await.status,
        200
    );
    for index in 0..4 {
        let previous = h
            .request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .json();
        let g = previous["generation"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let m = previous["metageneration"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap();
        let condition = match index {
            0 => format!("ifGenerationMatch={g}"),
            1 => format!("ifGenerationNotMatch={}", g + 1),
            2 => format!("ifMetagenerationMatch={m}"),
            _ => format!("ifMetagenerationNotMatch={}", m + 1),
        };
        let init = h
            .initiate(&format!("name=object&{condition}"), "", b"")
            .await;
        let path = h.path(&init);
        if index == 0 {
            assert_eq!(
                h.request(
                    "POST",
                    "/upload/storage/v1/b/bucket/o?uploadType=media&name=object",
                    "",
                    b"changed"
                )
                .await
                .status,
                200
            );
        } else if index == 1 {
            let replacement = h
                .request(
                    "POST",
                    "/upload/storage/v1/b/bucket/o?uploadType=media&name=object",
                    "",
                    b"changed",
                )
                .await
                .json();
            assert_eq!(replacement["generation"], (g + 1).to_string());
        } else {
            assert_eq!(
                h.request(
                    "PATCH",
                    "/storage/v1/b/bucket/o/object",
                    "Content-Type: application/json\r\n",
                    br#"{"cacheControl":"public,max-age=1"}"#
                )
                .await
                .status,
                200
            );
        }
        let current = h
            .request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .json();
        let current_bytes = h
            .request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
            .await
            .body;
        assert_eq!(
            h.request(
                "PUT",
                &format!("{path}&ifGenerationMatch=0"),
                "",
                b"forbidden override"
            )
            .await
            .status,
            400
        );
        let r = h.request("PUT", &path, "", b"failed replacement").await;
        assert_eq!(
            r.status,
            412,
            "{index}: {}",
            String::from_utf8_lossy(&r.body)
        );
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
                .await
                .json(),
            current
        );
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
                .await
                .body,
            current_bytes
        );
        assert_eq!(
            std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
            0
        );
        assert_eq!(
            std::fs::read_dir(h.dir.0.join("blobs"))
                .unwrap()
                .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
                .sum::<usize>(),
            1
        );
    }
    h.stop().await;
}

#[tokio::test]
async fn two_create_if_absent_sessions_have_one_finalization_winner() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let a = h.initiate("name=raced&ifGenerationMatch=0", "", b"").await;
    let a = h.path(&a);
    let b = h.initiate("name=raced&ifGenerationMatch=0", "", b"").await;
    let b = h.path(&b);
    let (a, b) = tokio::join!(
        h.request("PUT", &a, "", b"first"),
        h.request("PUT", &b, "", b"second")
    );
    let mut statuses = [a.status, b.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 412]);
    let winning = if a.status == 200 { a.json() } else { b.json() };
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/raced", "", b"")
            .await
            .json(),
        winning
    );
    h.stop().await;
}

#[tokio::test]
async fn capability_delegate_uses_initiator_grants_and_rechecks_overwrite_and_revocation() {
    let h = Harness::start(1024 * 1024, true, None).await;
    let init = h
        .initiate("name=delegated", "Authorization: Bearer creator\r\n", b"")
        .await;
    let path = h.path(&init);
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes */*\r\nAuthorization: Bearer invalid\r\n",
            b""
        )
        .await
        .status,
        308
    );
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Authorization: Bearer invalid\r\n",
            b"delegated"
        )
        .await
        .status,
        200
    );
    let init = h
        .initiate("name=raced", "Authorization: Bearer creator\r\n", b"")
        .await;
    let path = h.path(&init);
    let original = h
        .request(
            "POST",
            "/upload/storage/v1/b/bucket/o?uploadType=media&name=raced",
            "Authorization: Bearer admin\r\n",
            b"old",
        )
        .await
        .json();
    assert_eq!(
        h.request("PUT", &path, "", b"replacement").await.status,
        403
    );
    assert_eq!(
        h.request(
            "GET",
            "/storage/v1/b/bucket/o/raced",
            "Authorization: Bearer admin\r\n",
            b""
        )
        .await
        .json(),
        original
    );
    let grants=json!({"bindings":[{"role":"roles/storage.objectCreator","members":["user:viewer@example.test"]}]}).to_string();
    assert_eq!(
        h.request(
            "PUT",
            "/storage/v1/b/bucket/iam",
            "Authorization: Bearer admin\r\nContent-Type: application/json\r\n",
            grants.as_bytes()
        )
        .await
        .status,
        200
    );
    let init = h
        .initiate("name=revoked", "Authorization: Bearer viewer\r\n", b"")
        .await;
    assert_eq!(init.status, 200);
    let path = h.path(&init);
    assert_eq!(
        h.request(
            "PUT",
            "/storage/v1/b/bucket/iam",
            "Authorization: Bearer admin\r\nContent-Type: application/json\r\n",
            br#"{"bindings":[]}"#
        )
        .await
        .status,
        200
    );
    assert_eq!(h.request("PUT", &path, "", b"must fail").await.status, 403);
    h.stop().await;
    let h = Harness::start(1024 * 1024, true, None).await;
    let init = h
        .initiate("name=recreated", "Authorization: Bearer admin\r\n", b"")
        .await;
    let path = h.path(&init);
    assert_eq!(
        h.request(
            "DELETE",
            "/storage/v1/b/bucket",
            "Authorization: Bearer admin\r\n",
            b""
        )
        .await
        .status,
        204
    );
    assert_eq!(
        h.request(
            "POST",
            "/storage/v1/b?project=project",
            "Authorization: Bearer admin\r\nContent-Type: application/json\r\n",
            br#"{"name":"bucket"}"#
        )
        .await
        .status,
        200
    );
    assert_eq!(h.request("PUT", &path, "", b"must fail").await.status, 403);
    h.stop().await;
}

#[tokio::test]
async fn interrupted_chunk_releases_writer_and_preserves_only_acknowledged_bytes() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let init = h.initiate("name=object", "", b"").await;
    let path = h.path(&init);
    let chunk = vec![b'a'; 256 * 1024];
    assert_eq!(
        h.request("PUT", &path, "Content-Range: bytes 0-262143/*\r\n", &chunk)
            .await
            .status,
        308
    );
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 262144\r\nContent-Range: bytes 262144-524287/*\r\n\r\n").as_bytes()).await.unwrap();
    stream.write_all(&vec![b'b'; 64 * 1024]).await.unwrap();
    stream.shutdown().await.unwrap();
    drop(stream);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let r = h
            .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
            .await;
        if r.status != 503 {
            assert_eq!(r.status, 308);
            assert_eq!(r.headers["range"], "bytes=0-262143");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes 262144-524287/524288\r\n",
            &chunk
        )
        .await
        .status,
        200
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object?alt=media", "", b"")
            .await
            .body,
        vec![b'a'; 512 * 1024]
    );
    h.stop().await;
}

#[tokio::test]
async fn busy_session_rejects_competing_writer_while_other_sessions_progress() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let init = h.initiate("name=busy", "", b"").await;
    let path = h.path(&init);
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 262144\r\nContent-Range: bytes 0-262143/*\r\n\r\n").as_bytes()).await.unwrap();
    stream.write_all(b"a").await.unwrap();
    let id = path.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    let stage = h.dir.0.join("staging").join(session.blob_id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if tokio::fs::metadata(&stage).await.unwrap().len() > 0 {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        h.request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
            .await
            .status,
        503
    );
    let other = h.initiate("name=other", "", b"").await;
    let other = h.path(&other);
    assert_eq!(h.request("PUT", &other, "", b"other").await.status, 200);
    stream.shutdown().await.unwrap();
    drop(stream);
    h.stop().await;
}

#[tokio::test]
async fn sdk_initiation_accepts_one_retry_token_without_generation_conditions() {
    let h = Harness::start(1000, false, None).await;
    for _ in 0..2 {
        let r=h.initiate("name=object&alt=json&prettyPrint=false&projection=full","Content-Type: application/json\r\nX-Upload-Content-Type: application/octet-stream\r\nX-Goog-Gcs-Idempotency-Token: initiation-retry\r\n",b"{}").await;
        assert_eq!(r.status, 200);
        assert!(r.body.is_empty());
    }
    assert_eq!(
        std::fs::read_dir(h.dir.0.join("staging")).unwrap().count(),
        2
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/object", "", b"")
            .await
            .status,
        404
    );
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(b"POST /upload/storage/v1/b/bucket/o?uploadType=resumable&name=rejected HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain\r\nContent-Length: 999\r\n\r\n").await.unwrap();
    assert_eq!(Harness::read_response(stream).await.status, 400);
    h.stop().await;
}

#[tokio::test]
async fn host_follows_returned_location_and_streams_large_body_in_small_buffers() {
    let size = 8 * 1024 * 1024;
    let h = Harness::start(size, false, None).await;
    let init = h.initiate("name=streamed", "", b"").await;
    let uri: hyper::Uri = init.headers["location"].parse().unwrap();
    let mut stream = TcpStream::connect(uri.authority().unwrap().as_str())
        .await
        .unwrap();
    stream
        .write_all(
            format!(
                "PUT {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {size}\r\n\r\n",
                uri.path_and_query().unwrap(),
                uri.authority().unwrap()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let buffer = [b'x'; 64 * 1024];
    for _ in 0..128 {
        stream.write_all(&buffer).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    let r = Harness::read_response(stream).await;
    assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.json()["size"], size.to_string());
    let (_, mut reader) = h
        .state
        .catalog
        .admin(Origin::Seed)
        .open_object(
            &BucketName::new("bucket").unwrap(),
            &ObjectName::new("streamed").unwrap(),
            None,
        )
        .await
        .unwrap();
    let mut count = 0;
    while let Some(bytes) = reader.next_chunk().await.unwrap() {
        assert!(bytes.len() <= 64 * 1024);
        assert!(bytes.iter().all(|b| *b == b'x'));
        count += bytes.len();
    }
    assert_eq!(count as u64, size);
    drop(reader);
    h.stop().await;
}

#[tokio::test]
async fn chunked_cap_overflow_and_disconnected_transfers_release_all_body_slots() {
    let h = Harness::start(256 * 1024, false, None).await;
    let mut stalled = Vec::new();
    for i in 0..8 {
        let init = h.initiate(&format!("name=stalled{i}"), "", b"").await;
        let path = h.path(&init);
        let mut stream = TcpStream::connect(&h.address).await.unwrap();
        stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 262144\r\nContent-Range: bytes 0-262143/*\r\n\r\n").as_bytes()).await.unwrap();
        stream.write_all(&[b'x'; 64 * 1024]).await.unwrap();
        let id = path.split("upload_id=").nth(1).unwrap();
        let session = h
            .state
            .catalog
            .resumable_session(id, "bucket")
            .await
            .unwrap();
        let stage = h.dir.0.join("staging").join(session.blob_id);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::fs::metadata(&stage).await.unwrap().len() == 0 {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        stalled.push((stream, path));
    }
    let init = h.initiate("name=admission", "", b"").await;
    let path = h.path(&init);
    assert_eq!(h.request("PUT", &path, "", b"additional").await.status, 200);
    for (mut stream, path) in stalled {
        stream.shutdown().await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let r = h
                .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
                .await;
            if r.status != 503 {
                assert_eq!(r.status, 308);
                assert!(!r.headers.contains_key("range"));
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }
    let init = h.initiate("name=too-large", "", b"").await;
    let path = h.path(&init);
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\nContent-Range: bytes 0-262143/*\r\n\r\n40001\r\n").as_bytes()).await.unwrap();
    stream.write_all(&vec![b'y'; 256 * 1024 + 1]).await.unwrap();
    stream.write_all(b"\r\n0\r\n\r\n").await.unwrap();
    let r = Harness::read_response(stream).await;
    assert_eq!(r.status, 413, "{}", String::from_utf8_lossy(&r.body));
    let r = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(r.status, 308);
    assert!(!r.headers.contains_key("range"));
    h.stop().await;
}

#[tokio::test]
async fn final_metadata_and_completion_snapshot_survive_catalog_restart() {
    let h = Harness::start(1000, false, None).await;
    let init = h
        .initiate(
            "name=object",
            "Content-Type: application/json\r\n",
            br#"{"metadata":{"key":"initial"}}"#,
        )
        .await;
    let path = h.path(&init);
    let resource = h
        .request(
            "PUT",
            &path,
            "X-Goog-Meta-Key: final\r\nX-Goog-Hash: crc32c=mnG7TA==\r\n",
            b"hello",
        )
        .await
        .json();
    let id = path.split("upload_id=").nth(1).unwrap();
    h.state.catalog.close().await.unwrap();
    let catalog = Catalog::open_with_clock(&h.dir.0, h.clock.clone())
        .await
        .unwrap();
    let session = catalog.resumable_session(id, "bucket").await.unwrap();
    assert_eq!(session.completion, Some(resource));
    let record = catalog
        .admin(Origin::Seed)
        .get_object(
            &BucketName::new("bucket").unwrap(),
            &ObjectName::new("object").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        record.metadata.get("key").map(String::as_str),
        Some("final")
    );
    assert_eq!(record.crc32c, crc32c::crc32c(b"hello"));
    catalog.close().await.unwrap();
    h.stop().await;
}

#[tokio::test]
async fn session_deadline_during_transfer_returns_not_found_without_acknowledging_tail() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let init = h.initiate("name=expired", "", b"").await;
    let path = h.path(&init);
    let id = path.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 262144\r\nContent-Range: bytes 0-262143/*\r\n\r\n").as_bytes()).await.unwrap();
    stream.write_all(&[b'x'; 64 * 1024]).await.unwrap();
    let stage = h.dir.0.join("staging").join(session.blob_id);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::fs::metadata(&stage).await.unwrap().len() == 0 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    *h.clock.0.lock().unwrap() = session.deadline;
    for _ in 0..3 {
        stream.write_all(&[b'x'; 64 * 1024]).await.unwrap();
    }
    assert_eq!(Harness::read_response(stream).await.status, 404);
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/expired", "", b"")
            .await
            .status,
        404
    );
    h.stop().await;
}

#[tokio::test]
async fn strict_anonymous_cancellation_rejects_bodies_and_follows_original_deadline() {
    let h = Harness::start(1000, true, None).await;
    let init = h
        .initiate("name=cancelled", "Authorization: Bearer admin\r\n", b"")
        .await;
    let path = h.path(&init);
    let id = path.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    assert_eq!(h.request("DELETE", &path, "", b"bad").await.status, 400);
    assert_eq!(
        h.request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
            .await
            .status,
        308
    );
    let cancelled = h.request("DELETE", &path, "", b"").await;
    assert_eq!(cancelled.status, 499);
    assert!(cancelled.body.is_empty());
    assert_eq!(cancelled.headers["cache-control"], "no-store");
    assert!(!h.dir.0.join("staging").join(session.blob_id).exists());
    for method in ["PUT", "POST", "DELETE"] {
        assert_eq!(
            h.request(method, &path, "Content-Range: bytes */*\r\n", b"")
                .await
                .status,
            410
        );
    }
    *h.clock.0.lock().unwrap() = session.deadline;
    for method in ["PUT", "POST", "DELETE"] {
        assert_eq!(
            h.request(method, &path, "Content-Range: bytes */*\r\n", b"")
                .await
                .status,
            404
        );
    }
    let unknown = format!(
        "/upload/storage/v1/b/bucket/o?uploadType=resumable&upload_id={}",
        "0".repeat(64)
    );
    assert_eq!(h.request("DELETE", &unknown, "", b"").await.status, 404);
    h.stop().await;
}

#[tokio::test]
async fn cancellation_racing_append_respects_writer_and_prevents_later_publication() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let init = h.initiate("name=append", "", b"").await;
    let path = h.path(&init);
    let id = path.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    let stage = h.dir.0.join("staging").join(session.blob_id);
    let mut stream = TcpStream::connect(&h.address).await.unwrap();
    stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 262144\r\nContent-Range: bytes 0-262143/*\r\n\r\n").as_bytes()).await.unwrap();
    stream.write_all(&[b'a'; 64 * 1024]).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::fs::metadata(&stage).await.unwrap().len() == 0 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    assert_eq!(h.request("DELETE", &path, "", b"").await.status, 503);
    for _ in 0..3 {
        stream.write_all(&[b'a'; 64 * 1024]).await.unwrap();
    }
    assert_eq!(Harness::read_response(stream).await.status, 308);
    assert_eq!(h.request("DELETE", &path, "", b"").await.status, 499);
    assert_eq!(
        h.request(
            "PUT",
            &path,
            "Content-Range: bytes 262144-262144/262145\r\n",
            b"b"
        )
        .await
        .status,
        410
    );
    assert!(!stage.exists());
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/append", "", b"")
            .await
            .status,
        404
    );
    h.stop().await;
}

#[tokio::test]
async fn cancellation_racing_finalization_has_one_terminal_winner_and_preserves_live_bytes() {
    let h = Harness::start(1024 * 1024, false, None).await;
    for index in 0..8 {
        let name = format!("race-{index}");
        let init = h.initiate(&format!("name={name}"), "", b"").await;
        let path = h.path(&init);
        let (finalized, cancelled) = tokio::join!(
            h.request("PUT", &path, "", b"completed bytes"),
            h.request("DELETE", &path, "", b""),
        );
        assert_ne!(finalized.status == 200, cancelled.status == 499);
        if finalized.status == 200 {
            assert!(matches!(cancelled.status, 410 | 503));
            assert_eq!(h.request("DELETE", &path, "", b"").await.status, 410);
        } else {
            assert_eq!(cancelled.status, 499);
            assert!(matches!(finalized.status, 410 | 503));
        }
        let id = path.split("upload_id=").nth(1).unwrap();
        // Expiring a completed capability must not delete the published object.
        if finalized.status == 200 {
            let session = h
                .state
                .catalog
                .resumable_session(id, "bucket")
                .await
                .unwrap();
            *h.clock.0.lock().unwrap() = session.deadline;
        } else {
            *h.clock.0.lock().unwrap() += Duration::from_secs(604800);
        }
        h.state.catalog.maintain_resumable_sessions().await.unwrap();
        let object = h
            .request(
                "GET",
                &format!("/storage/v1/b/bucket/o/{name}?alt=media"),
                "",
                b"",
            )
            .await;
        if finalized.status == 200 {
            assert_eq!(object.status, 200);
            assert_eq!(object.body, b"completed bytes");
        } else {
            assert_eq!(object.status, 404);
        }
    }
    h.stop().await;
}

#[tokio::test]
async fn failed_finalization_and_cancel_cleanup_retry_before_session_deadline() {
    let h = Harness::start(1000, false, None).await;
    h.state.catalog.start_resumable_maintenance();
    for cancel in [false, true] {
        let init = h.initiate(&format!("name=cleanup-{cancel}"), "", b"").await;
        let path = h.path(&init);
        let id = path.split("upload_id=").nth(1).unwrap();
        let session = h
            .state
            .catalog
            .resumable_session(id, "bucket")
            .await
            .unwrap();
        let stage = h.dir.0.join("staging").join(session.blob_id);
        // Let the scheduler arm the seven-day timer before injecting the failed unlink.
        tokio::time::sleep(Duration::from_millis(50)).await;
        std::fs::remove_file(&stage).unwrap();
        std::fs::create_dir(&stage).unwrap();
        let response = if cancel {
            h.request("DELETE", &path, "", b"").await
        } else {
            h.request("PUT", &path, "X-Goog-Hash: crc32c=invalid\r\n", b"")
                .await
        };
        assert_eq!(response.status, if cancel { 499 } else { 400 });
        // A hang guard shared by the polling loops below; each exits as soon as its
        // state appears, and a loaded runner has needed more than three seconds.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while h.state.catalog.resumable_maintenance_error().is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "cleanup scheduler was not woken"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        loop {
            let status = h.request("DELETE", &path, "", b"").await.status;
            if status != 503 {
                assert_eq!(status, 410);
                break;
            }
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        replace_resumable_cleanup_obstacle(&h.state.catalog, stage.clone()).await;
        while stage.exists() || h.state.catalog.resumable_maintenance_error().is_some() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(h.clock.now() < session.deadline);
        assert_eq!(
            h.request(
                "GET",
                &format!("/storage/v1/b/bucket/o/cleanup-{cancel}"),
                "",
                b""
            )
            .await
            .status,
            404
        );
    }
    h.stop().await;
}

#[tokio::test]
async fn acknowledged_prefix_resumes_through_router_after_restart_and_expired_writes_fail() {
    let mut h = Harness::start(1024 * 1024, false, None).await;
    let init = h.initiate("name=resumed", "", b"").await;
    let path = h.path(&init);
    let chunk = vec![b'a'; 256 * 1024];
    assert_eq!(
        h.request("PUT", &path, "Content-Range: bytes 0-262143/*\r\n", &chunk)
            .await
            .status,
        308
    );
    h.stop.send(()).unwrap();
    h.server.await.unwrap().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    h.address = listener.local_addr().unwrap().to_string();
    h.base = format!("http://{}", h.address);
    h.state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: h.clock.clone(),
        catalog: Catalog::open_with_clock(&h.dir.0, h.clock.clone())
            .await
            .unwrap(),
        api_base_url: h.base.clone(),
        default_location: "US".into(),
        auth: Auth::permissive(),
    });
    let config = Config {
        scheme: gcs_mimic::config::Scheme::Http,
        api_base_url: Some(h.base.clone()),
        max_object_bytes: 1024 * 1024,
        ..Default::default()
    };
    let (stop, rx) = oneshot::channel();
    let state = h.state.clone();
    h.stop = stop;
    h.server = tokio::spawn(async move {
        serve(listener, config, state, async {
            rx.await.unwrap();
            Ok(())
        })
        .await
    });
    let status = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(status.status, 308);
    assert_eq!(status.headers["range"], "bytes=0-262143");
    assert_eq!(
        h.request(
            "POST",
            &path,
            "Content-Range: bytes 262144-262144/262145\r\n",
            b"b"
        )
        .await
        .status,
        200
    );
    let mut expected = chunk;
    expected.push(b'b');
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/resumed?alt=media", "", b"")
            .await
            .body,
        expected
    );
    let init = h.initiate("name=expired-write", "", b"").await;
    let expired = h.path(&init);
    let id = expired.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    *h.clock.0.lock().unwrap() = session.deadline;
    assert_eq!(
        h.request("PUT", &expired, "", b"no publication")
            .await
            .status,
        404
    );
    assert_eq!(
        h.request("PUT", &expired, "Content-Range: bytes */*\r\n", b"")
            .await
            .status,
        404
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/expired-write", "", b"")
            .await
            .status,
        404
    );
    h.stop().await;
}

#[tokio::test]
async fn retryable_publish_and_commit_failures_preserve_the_prefix_and_allocate_one_generation() {
    for fail_commit in [false, true] {
        let h = Harness::start(1024 * 1024, false, None).await;
        let init = h.initiate("name=retry", "", b"").await;
        let path = h.path(&init);
        let id = path.split("upload_id=").nth(1).unwrap();
        let chunk = vec![b'x'; 256 * 1024];
        assert_eq!(
            h.request("PUT", &path, "Content-Range: bytes 0-262143/*\r\n", &chunk)
                .await
                .status,
            308
        );
        let session = h
            .state
            .catalog
            .resumable_session(id, "bucket")
            .await
            .unwrap();
        let shard = h.dir.0.join("blobs").join(&session.blob_id[..2]);
        let connection = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
        let previous_generation: i64 = connection
            .query_row("SELECT last_generation FROM object_generation", [], |row| {
                row.get(0)
            })
            .unwrap();
        if fail_commit {
            connection.execute_batch("CREATE TRIGGER fail_publication BEFORE INSERT ON objects BEGIN SELECT RAISE(FAIL, 'injected publication failure'); END;").unwrap();
        } else {
            std::fs::write(&shard, b"injected shard creation failure").unwrap();
        }
        assert_eq!(
            h.request("PUT", &path, "Content-Range: bytes */262144\r\n", b"")
                .await
                .status,
            503
        );
        let status = h
            .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
            .await;
        assert_eq!(status.status, 308);
        assert_eq!(status.headers["range"], "bytes=0-262143");
        assert_eq!(
            connection
                .query_row("SELECT last_generation FROM object_generation", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            previous_generation
        );
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/retry", "", b"")
                .await
                .status,
            404
        );
        if fail_commit {
            connection
                .execute_batch("DROP TRIGGER fail_publication;")
                .unwrap();
        } else {
            std::fs::remove_file(&shard).unwrap();
        }
        let completed = h
            .request(
                "PUT",
                &path,
                "Content-Range: bytes 0-262143/262144\r\n",
                &chunk,
            )
            .await;
        assert_eq!(completed.status, 200);
        let resource = completed.json();
        let generation: i64 = resource["generation"].as_str().unwrap().parse().unwrap();
        assert!(generation > previous_generation);
        let repeated = h
            .request("PUT", &path, "Content-Range: bytes */262144\r\n", b"")
            .await;
        assert_eq!(repeated.status, 200);
        assert_eq!(repeated.json(), resource);
        assert_eq!(
            connection
                .query_row("SELECT last_generation FROM object_generation", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            generation
        );
        assert_eq!(
            h.request("GET", "/storage/v1/b/bucket/o/retry?alt=media", "", b"")
                .await
                .body,
            chunk
        );
        // Close the test's own catalog handle first; `stop` removes the directory.
        drop(connection);
        h.stop().await;
    }
}

#[tokio::test]
async fn contradictory_chunk_lengths_fail_before_reading_body_or_acquiring_session() {
    let h = Harness::start(1024 * 1024, false, None).await;
    let init = h.initiate("name=length", "", b"").await;
    let path = h.path(&init);
    let id = path.split("upload_id=").nth(1).unwrap();
    let lease = h
        .state
        .catalog
        .lock_resumable_session(id, "bucket")
        .await
        .unwrap();
    let stage = lease.staging_path();
    for declared in [1, 262145] {
        let mut stream = TcpStream::connect(&h.address).await.unwrap();
        stream.write_all(format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {declared}\r\nContent-Range: bytes 0-262143/*\r\n\r\n").as_bytes()).await.unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(1), Harness::read_response(stream))
            .await
            .unwrap();
        assert_eq!(reply.status, 400);
        assert_eq!(
            reply.json()["error"]["errors"][0]["reason"],
            "invalidParameter"
        );
    }
    assert_eq!(std::fs::metadata(stage).unwrap().len(), 0);
    drop(lease);
    let status = h
        .request("PUT", &path, "Content-Range: bytes */*\r\n", b"")
        .await;
    assert_eq!(status.status, 308);
    assert!(!status.headers.contains_key("range"));
    h.stop().await;
}

#[tokio::test]
async fn final_hash_scan_uses_request_budget_instead_of_catalog_wait_budget() {
    // Which deadline applies during the scan is pinned in virtual time by the
    // `await_session_upload` tests. Here a 32 MiB scan runs under a short catalog
    // budget end to end; catalog steps that exceed it on a loaded runner are
    // retried because a completed upload may still commit.
    let size = 32 * 1024 * 1024;
    let h = Harness::start_with_catalog_wait(size, false, None, 200).await;
    let init = retry_unavailable(|| h.initiate("name=hash-scan", "", b"")).await;
    let path = h.path(&init);
    let bytes = vec![b'h'; size as usize];
    let range = format!("Content-Range: bytes 0-{}/*\r\n", size - 1);
    assert_eq!(
        retry_unavailable(|| h.request("PUT", &path, &range, &bytes))
            .await
            .status,
        308
    );
    let started = tokio::time::Instant::now();
    let mut completed = h
        .request(
            "PUT",
            &path,
            &format!("Content-Range: bytes */{size}\r\n"),
            b"",
        )
        .await;
    eprintln!(
        "32 MiB final hash scan and publication: {:?}; catalog wait budget: 200 ms",
        started.elapsed()
    );
    if completed.status == 503 {
        assert_catalog_wait_timeout(&completed);
        completed =
            retry_unavailable(|| h.request("PUT", &path, "Content-Range: bytes */*\r\n", b""))
                .await;
    }
    assert_eq!(completed.status, 200);
    assert_eq!(completed.json()["size"], size.to_string());
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/hash-scan?alt=media", "", b"")
            .await
            .body,
        bytes
    );
    h.stop().await;
}

#[tokio::test]
async fn finalization_catalog_wait_is_bounded_and_publication_may_finish_after_timeout() {
    // Only the blocked finalization is under test; earlier steps may also exceed
    // the short budget on a loaded runner, so retry their transient 503s.
    let h = Harness::start_with_catalog_wait(1024 * 1024, false, None, 250).await;
    let init = retry_unavailable(|| h.initiate("name=sql-wait", "", b"")).await;
    let path = h.path(&init);
    let bytes = vec![b's'; 256 * 1024];
    assert_eq!(
        retry_unavailable(|| h.request(
            "PUT",
            &path,
            "Content-Range: bytes 0-262143/*\r\n",
            &bytes
        ))
        .await
        .status,
        308
    );
    let database = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
    database.execute_batch("BEGIN IMMEDIATE;").unwrap();
    // The write lock holds the finalization in its catalog wait, so the wait's
    // lower bound does not depend on runner speed. The error message shows the
    // catalog budget, not the 900 s request deadline, ended it; the timeout is
    // only a hang guard.
    let started = tokio::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(30),
        h.request("PUT", &path, "Content-Range: bytes */262144\r\n", b""),
    )
    .await
    .unwrap();
    assert_catalog_wait_timeout(&response);
    assert!(started.elapsed() >= Duration::from_millis(250));
    database.execute_batch("COMMIT;").unwrap();
    assert_eq!(
        retry_unavailable(|| h.request("PUT", &path, "Content-Range: bytes */*\r\n", b""))
            .await
            .status,
        200
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/sql-wait?alt=media", "", b"")
            .await
            .body,
        bytes
    );
    // Close the test's own catalog handle first; `stop` removes the directory.
    drop(database);
    h.stop().await;
}

// The injected file at the shard path fails cleanup with ENOTDIR on Unix; Windows
// reports the missing parent as not found, which cleanup treats as already removed.
#[cfg(unix)]
#[tokio::test]
async fn checksum_error_is_preserved_when_terminal_cleanup_fails_and_is_retried() {
    let h = Harness::start(1000, false, None).await;
    let init = h
        .initiate("name=bad-checksum", "X-Goog-Hash: crc32c=AAAAAA==\r\n", b"")
        .await;
    let path = h.path(&init);
    let id = path.split("upload_id=").nth(1).unwrap();
    let session = h
        .state
        .catalog
        .resumable_session(id, "bucket")
        .await
        .unwrap();
    let shard = h.dir.0.join("blobs").join(&session.blob_id[..2]);
    std::fs::write(&shard, b"cleanup failure").unwrap();
    let response = h.request("PUT", &path, "", b"hello").await;
    assert_eq!(response.status, 400);
    assert_eq!(response.json()["error"]["errors"][0]["reason"], "invalid");
    assert!(h.state.catalog.resumable_maintenance_error().is_some());
    assert_eq!(h.request("DELETE", &path, "", b"").await.status, 410);
    let connection = rusqlite::Connection::open(h.dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM pending_blob_deletions WHERE blob_id = ?1",
                [&session.blob_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    std::fs::remove_file(shard).unwrap();
    h.state.catalog.maintain_resumable_sessions().await.unwrap();
    assert!(h.state.catalog.resumable_maintenance_error().is_none());
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM pending_blob_deletions WHERE blob_id = ?1",
                [&session.blob_id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        h.request("GET", "/storage/v1/b/bucket/o/bad-checksum", "", b"")
            .await
            .status,
        404
    );
    h.stop().await;
}
