#[allow(dead_code)]
mod common;
use common::TestDir;
use gcs_mimic::{
    auth::{Auth, Registry},
    bucket::BucketRecord,
    clock::{Clock, SystemClock},
    config::Config,
    server::{State, serve},
    storage::Catalog,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
    time::UNIX_EPOCH,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::JoinHandle,
};

struct Harness {
    dir: TestDir,
    address: String,
    base: String,
    state: Arc<State>,
    stop: oneshot::Sender<()>,
    server: JoinHandle<std::io::Result<()>>,
}
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}
impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}
impl Harness {
    async fn start(max: u64, strict: bool, base: Option<&str>) -> Self {
        let dir = TestDir::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let base = base
            .map(str::to_owned)
            .unwrap_or_else(|| format!("http://{address}"));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
            .await
            .unwrap();
        catalog
            .insert_bucket(BucketRecord {
                name: "bucket".into(),
                project: "project".into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                labels: Default::default(),
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
        let auth = if strict {
            Auth::strict(Registry::parse(json!({"identities":[{"id":"user:admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},{"id":"user:creator@example.test","tokens":["creator"],"project_roles":{"project":["roles/storage.objectCreator"]}},{"id":"user:viewer@example.test","tokens":["viewer"],"project_roles":{"project":["roles/storage.objectViewer"]}}]}).to_string().as_bytes()).unwrap())
        } else {
            Auth::permissive()
        };
        let state = Arc::new(State {
            ready: AtomicBool::new(true),
            clock,
            catalog,
            api_base_url: base.trim_end_matches('/').into(),
            auth,
        });
        let config = Config {
            api_base_url: state.api_base_url.clone(),
            max_object_bytes: max,
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
            stop,
            server,
        }
    }
    async fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let mut stream = TcpStream::connect(&self.address).await.unwrap();
        stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: spoofed.invalid\r\nForwarded: host=evil.invalid;proto=https\r\nX-Forwarded-Host: evil.invalid\r\nContent-Length: {}\r\n{headers}\r\n",body.len()).as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
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
        ("name=a", "X-Goog-Upload-Command: start\r\n", b"", 501),
        (
            "name=a",
            "X-Goog-Gcs-Idempotency-Token: retry\r\n",
            b"",
            501,
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
