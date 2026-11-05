use crate::common;
use common::http::Body;

use common::effects::{Reply, request};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    path::Path,
    process::{Child, ChildStdout, Stdio},
};

const AUTH: &str = "Authorization: Bearer admin\r\n";
const OBJECT: &str = "/storage/v1/b/test-bucket/o/value";

#[derive(Clone, Copy, PartialEq, Eq)]
enum SoftDeleteEnv {
    Pinned,
    Unset,
}

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
    grpc_address: Option<String>,
    stopped: bool,
}

impl Server {
    fn start(data: &Path, options: &[&str]) -> Self {
        Self::start_with(data, options, SoftDeleteEnv::Pinned)
    }

    fn start_with(data: &Path, options: &[&str], env: SoftDeleteEnv) -> Self {
        let mut command = common::server_command();
        if env == SoftDeleteEnv::Unset {
            // The suite pins the default to 0; the memory default is only reachable without it.
            command.env_remove("GCS_MIMIC_DEFAULT_SOFT_DELETE_RETENTION_SECONDS");
        }
        let mut child = command
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--backend",
                "memory",
                "--data-dir",
            ])
            .arg(data)
            .args(options)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready", "{ready}");
        assert_eq!(ready["backend"], "memory");
        assert!(ready["memory_max_bytes"].as_u64().unwrap() > 0);
        assert!(ready["memory_catalog_max_bytes"].as_u64().unwrap() >= 1024 * 1024);
        Self {
            child,
            stdout,
            address: ready["listen"].as_str().unwrap().into(),
            grpc_address: ready["listeners"]
                .as_array()
                .and_then(|listeners| {
                    listeners
                        .iter()
                        .find(|listener| listener["scheme"] == "h2c")
                })
                .and_then(|listener| listener["listen"].as_str())
                .map(str::to_owned),
            stopped: false,
        }
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, headers, body)
    }

    fn bucket(&self, versioning: bool) {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            AUTH,
            json!({"name":"test-bucket", "versioning":{"enabled":versioning}})
                .to_string()
                .as_bytes(),
        )
        .json_ok();
    }

    fn put(&self, body: &[u8]) -> Reply {
        self.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            AUTH,
            body,
        )
    }

    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
        self.stopped = true;
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[tokio::test]
async fn memory_grpc_listener_serves_protobuf_over_http2_without_opening_data_dir() {
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    let dir = common::TestDir::new();
    let data = dir.0.join("never-created");
    let mut server = Server::start(&data, &["--grpc-listen", "127.0.0.1:0"]);
    server.bucket(false);
    let address = server.grpc_address.as_ref().unwrap();
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let (mut client, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
            .await
            .unwrap();
    let driver = tokio::spawn(connection);
    let name = b"projects/_/buckets/test-bucket";
    // GetBucketRequest.name is protobuf field 1 in the pinned Storage v2 definition.
    let mut message = vec![0x0a, name.len() as u8];
    message.extend_from_slice(name);
    let mut framed = vec![0];
    framed.extend_from_slice(&(message.len() as u32).to_be_bytes());
    framed.extend_from_slice(&message);
    let response = client
        .send_request(
            hyper::Request::builder()
                .method("POST")
                .uri(format!(
                    "http://{address}/google.storage.v2.Storage/GetBucket"
                ))
                .header("content-type", "application/grpc")
                .header("te", "trailers")
                .body(Full::new(Bytes::from(framed)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.version(), hyper::Version::HTTP_2);
    assert_eq!(response.status(), 200);
    let body = response.into_body().collect().await.unwrap();
    assert_eq!(body.trailers().unwrap()["grpc-status"], "0");
    let bytes = body.to_bytes();
    assert_eq!(bytes[0], 0);
    assert_eq!(
        u32::from_be_bytes(bytes[1..5].try_into().unwrap()) as usize,
        bytes.len() - 5
    );
    assert!(bytes.windows(name.len()).any(|window| window == name));
    drop(client);
    driver.abort();
    let _ = driver.await;
    assert!(!data.exists());
    server.stop();
    assert!(!data.exists());
}

#[test]
fn memory_wire_preserves_shared_permissions_conditions_history_and_never_opens_data_dir() {
    let dir = common::TestDir::new();
    let data = dir.0.join("persistent-data");
    fs::write(&data, b"untouched existing data").unwrap();
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"local":["roles/storage.objectViewer"]}}
        ]}),
    );
    let mut server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    server.bucket(true);
    let first = server.put(b"old\0bytes").json_ok();
    let generation = first["generation"].as_str().unwrap();
    server
        .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
        .json_reason(401, "required");
    assert_eq!(
        server
            .request(
                "GET",
                &format!("{OBJECT}?alt=media"),
                "Authorization: Bearer viewer\r\n",
                b""
            )
            .body,
        b"old\0bytes"
    );
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            "Authorization: Bearer viewer\r\n",
            b"denied",
        )
        .json_reason(403, "forbidden");
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value&ifGenerationMatch=0",
            AUTH,
            b"wrong",
        )
        .json_reason(412, "conditionNotMet");
    let second = server.put(b"new bytes").json_ok();
    assert_ne!(first["generation"], second["generation"]);
    assert_eq!(
        server
            .request(
                "GET",
                &format!("{OBJECT}?alt=media&generation={generation}"),
                AUTH,
                b""
            )
            .body,
        b"old\0bytes"
    );
    let range = server.request(
        "GET",
        &format!("{OBJECT}?alt=media"),
        &format!("{AUTH}Range: bytes=1-3\r\n"),
        b"",
    );
    range.expect(206);
    assert_eq!(range.body, b"ew ");
    let listed = server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o?versions=true",
            AUTH,
            b"",
        )
        .json_ok();
    assert_eq!(listed["items"].as_array().unwrap().len(), 2);
    server
        .request("PATCH", OBJECT, AUTH, br#"{"metadata":{"ram":"yes"}}"#)
        .json_ok();
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["metadata"]["ram"],
        "yes"
    );
    server
        .request("PUT", "/test-bucket/xml-object", AUTH, b"XML bytes")
        .expect(200);
    assert_eq!(
        server
            .request("GET", "/test-bucket/xml-object", AUTH, b"")
            .body,
        b"XML bytes"
    );
    let multipart = b"--memory\r\nContent-Type: application/json\r\n\r\n{\"name\":\"multipart\",\"metadata\":{\"ram\":\"yes\"}}\r\n--memory\r\nContent-Type: text/plain\r\n\r\nmultipart bytes\r\n--memory--\r\n";
    let mut stream = TcpStream::connect(&server.address).unwrap();
    write!(stream, "POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=memory\r\nContent-Length: {}\r\n{AUTH}Connection: close\r\n\r\n", multipart.len()).unwrap();
    stream.write_all(multipart).unwrap();
    let response = common::read_http_response(&mut BufReader::new(stream), Body::Expected).unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 200 "),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/multipart?alt=media",
                AUTH,
                b""
            )
            .body,
        b"multipart bytes"
    );
    let independent = Server::start(&data, &[]);
    independent
        .request("GET", OBJECT, "", b"")
        .json_reason(404, "notFound");
    server.stop();
    let restarted = Server::start(&data, &[]);
    restarted
        .request("GET", OBJECT, "", b"")
        .json_reason(404, "notFound");
    assert_eq!(fs::read(&data).unwrap(), b"untouched existing data");
}

#[test]
fn memory_derived_wire_shares_strict_permissions_history_compose_and_rewrite_tokens() {
    let dir = common::TestDir::new();
    let data = dir.0.join("not-created");
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"local":["roles/storage.objectViewer"]}}
        ]}),
    );
    let mut server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    server.bucket(true);
    let body = vec![b'x'; 2 * 1024 * 1024 + 7];
    let source = server.put(&body).json_ok();
    let copy_path = "/storage/v1/b/test-bucket/o/value/copyTo/b/test-bucket/o/copy";
    server
        .request("POST", copy_path, "Authorization: Bearer viewer\r\n", b"")
        .json_reason(403, "forbidden");
    server
        .request(
            "POST",
            &format!("{copy_path}?ifSourceGenerationMatch=0"),
            AUTH,
            b"",
        )
        .json_reason(412, "conditionNotMet");
    let copy = server
        .request(
            "POST",
            &format!("{copy_path}?ifGenerationMatch=0"),
            AUTH,
            br#"{"metadata":{"kind":"copy"}}"#,
        )
        .json_ok();
    assert_eq!(copy["md5Hash"], source["md5Hash"]);
    assert_eq!(copy["crc32c"], source["crc32c"]);
    assert_eq!(copy["metadata"]["kind"], "copy");
    server
        .request(
            "POST",
            &format!("{copy_path}?ifGenerationMatch=0"),
            AUTH,
            b"",
        )
        .json_reason(412, "conditionNotMet");
    let composed = server.request("POST", "/storage/v1/b/test-bucket/o/composite/compose?ifGenerationMatch=0", AUTH,
        json!({"sourceObjects":[{"name":"copy","generation":copy["generation"]},{"name":"value","generation":source["generation"]}],"destination":{"metadata":{"kind":"composite"}}}).to_string().as_bytes()).json_ok();
    assert_eq!(composed["size"], (body.len() * 2).to_string());
    assert_eq!(composed["componentCount"], 2);
    assert!(composed.get("md5Hash").is_none());
    let bytes = server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/composite?alt=media",
            AUTH,
            b"",
        )
        .body;
    assert_eq!(&bytes[..body.len()], &body);
    assert_eq!(&bytes[body.len()..], &body);
    let base = "/storage/v1/b/test-bucket/o/value/rewriteTo/b/test-bucket/o/rewritten";
    let first = server
        .request(
            "POST",
            &format!("{base}?maxBytesRewrittenPerCall=1048576&ifGenerationMatch=0"),
            AUTH,
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json_ok();
    assert_eq!(first["done"], false);
    assert_eq!(first["totalBytesRewritten"], "1048576");
    let next = format!(
        "{base}?rewriteToken={}",
        first["rewriteToken"].as_str().unwrap()
    );
    let second = server.request("POST", &next, AUTH, b"").json_ok();
    assert_eq!(second["done"], false);
    assert_eq!(server.request("POST", &next, AUTH, b"").json_ok(), second);
    let last = format!(
        "{base}?rewriteToken={}",
        second["rewriteToken"].as_str().unwrap()
    );
    let completed = server.request("POST", &last, AUTH, b"").json_ok();
    assert_eq!(completed["done"], true);
    assert_eq!(completed["resource"]["md5Hash"], source["md5Hash"]);
    assert_eq!(
        server.request("POST", &last, AUTH, b"").json_ok(),
        completed
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/rewritten?alt=media",
                AUTH,
                b""
            )
            .body,
        body
    );
    server
        .request(
            "PUT",
            "/test-bucket/xml-copy",
            &format!("{AUTH}x-goog-copy-source: /test-bucket/copy\r\n"),
            b"",
        )
        .expect(200);
    assert_eq!(
        server
            .request("GET", "/test-bucket/xml-copy", AUTH, b"")
            .body,
        body
    );
    let overwritten = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/copy/copyTo/b/test-bucket/o/copy",
            AUTH,
            b"",
        )
        .json_ok();
    assert_ne!(overwritten["generation"], copy["generation"]);
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "/storage/v1/b/test-bucket/o/copy?alt=media&generation={}",
                    copy["generation"].as_str().unwrap()
                ),
                AUTH,
                b""
            )
            .body,
        body
    );
    let disposal =
        br#"{"sourceObjects":[{"name":"copy"},{"name":"value"}],"deleteSourceObjects":true}"#;
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/disposed/compose?ifGenerationMatch=1",
            AUTH,
            disposal,
        )
        .json_reason(412, "conditionNotMet");
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        source["generation"]
    );
    let disposed = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/disposed/compose?ifGenerationMatch=0",
            AUTH,
            disposal,
        )
        .json_ok();
    assert_eq!(disposed["componentCount"], 2);
    server
        .request("GET", OBJECT, AUTH, b"")
        .json_reason(404, "notFound");
    server
        .request("GET", "/storage/v1/b/test-bucket/o/copy", AUTH, b"")
        .json_reason(404, "notFound");
    assert!(!data.exists());
    server.stop();
    assert!(!data.exists());
}

#[test]
fn memory_capacity_aborted_input_and_unsupported_sessions_preserve_old_state() {
    let dir = common::TestDir::new();
    let data = dir.0.join("never-created");
    let mut server = Server::start(
        &data,
        &["--memory-max-bytes", "260", "--admin-enabled", "true"],
    );
    server.bucket(false);
    let first = server.put(b"old!").json_ok();
    let capacity = server.put(b"new!");
    capacity.json_reason(503, "backendError");
    let message: Value = serde_json::from_slice(&capacity.body).unwrap();
    assert!(
        message["error"]["message"]
            .as_str()
            .unwrap()
            .contains("memory_max_bytes")
    );
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        first["generation"]
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), AUTH, b"")
            .body,
        b"old!"
    );
    for path in [
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=session",
        "/storage/v1/b/test-bucket/o/value/copyTo/b/test-bucket/o/copy",
        "/storage/v1/b/test-bucket/o/value/rewriteTo/b/test-bucket/o/copy",
        "/storage/v1/b/test-bucket/o/copy/compose",
    ] {
        let response = server.request(
            "POST",
            path,
            AUTH,
            if path.ends_with("compose") {
                br#"{"sourceObjects":[{"name":"value"}]}"#
            } else {
                b"{}"
            },
        );
        response.json_reason(503, "backendError");
        assert_eq!(
            server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
            first["generation"]
        );
    }
    let parts = server.request("POST", "/test-bucket/value?uploads", "", b"");
    parts.expect(200);
    let parts_xml = String::from_utf8(parts.body).unwrap();
    let upload_id = parts_xml
        .split_once("<UploadId>")
        .unwrap()
        .1
        .split_once("</UploadId>")
        .unwrap()
        .0;
    server
        .request(
            "PUT",
            &format!("/test-bucket/value?uploadId={upload_id}&partNumber=1"),
            "",
            b"full",
        )
        .expect(503);
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        first["generation"]
    );
    server
        .request(
            "DELETE",
            &format!("/test-bucket/value?uploadId={upload_id}"),
            "",
            b"",
        )
        .expect(204);
    server.request("DELETE", OBJECT, AUTH, b"").expect(204);
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=value HTTP/1.1\r\nHost: localhost\r\nContent-Length: 8\r\n\r\ntail").unwrap();
    drop(stream);
    // Reset takes the existing activity gate, so wait only for an incomplete request's cleanup.
    common::eventually("reset after the incomplete upload's cleanup", || {
        let response = server.request(
            "POST",
            "/_internal/reset",
            "",
            br#"{"confirm":"gcs-mimic"}"#,
        );
        if response.status == 200 {
            return true;
        }
        response.expect(503);
        false
    });
    server.bucket(false);
    server.put(b"next").json_ok();
    server.stop();
    assert!(!data.exists());
}

#[test]
fn memory_soft_delete_restore_uses_shared_wire_conditions_and_permissions() {
    let dir = common::TestDir::new();
    let data = dir.0.join("never-created");
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"local":["roles/storage.objectViewer"]}}
        ]}),
    );
    let mut server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    server.request("POST", "/storage/v1/b?project=local", AUTH, json!({"name":"test-bucket", "versioning":{"enabled":true}, "softDeletePolicy":{"retentionDurationSeconds":"604800"}}).to_string().as_bytes()).json_ok();
    let first = server.put(b"retained body").json_ok();
    let original_generation = first["generation"].as_str().unwrap();
    let selected = format!("{OBJECT}?generation={original_generation}");
    server.request("DELETE", &selected, AUTH, b"").expect(204);
    let deleted = server
        .request("GET", &format!("{selected}&softDeleted=true"), AUTH, b"")
        .json_ok();
    assert!(deleted["softDeleteTime"].is_string());
    let restore = format!("{OBJECT}/restore?generation={original_generation}");
    server
        .request("POST", &restore, "", b"")
        .json_reason(401, "required");
    server
        .request("POST", &restore, "Authorization: Bearer viewer\r\n", b"")
        .json_reason(403, "forbidden");
    server
        .request("POST", &format!("{restore}&copySourceAcl=true"), AUTH, b"")
        .json_reason(400, "badRequest");
    server
        .request(
            "POST",
            &format!("{restore}&restoreToken=unsupported"),
            AUTH,
            b"",
        )
        .json_reason(400, "badRequest");
    let restored = server
        .request("POST", &format!("{restore}&ifGenerationMatch=0"), AUTH, b"")
        .json_ok();
    assert_ne!(restored["generation"], first["generation"]);
    assert_eq!(restored["metageneration"], "1");
    assert!(restored["softDeleteTime"].is_null());
    assert!(restored["hardDeleteTime"].is_null());
    assert_eq!(restored["md5Hash"], first["md5Hash"]);
    assert_eq!(restored["crc32c"], first["crc32c"]);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), AUTH, b"")
            .body,
        b"retained body"
    );
    assert_eq!(
        server
            .request("GET", &format!("{selected}&softDeleted=true"), AUTH, b"")
            .json_ok(),
        deleted
    );
    server
        .request("POST", &format!("{restore}&ifGenerationMatch=0"), AUTH, b"")
        .json_reason(412, "conditionNotMet");
    let again = server
        .request(
            "POST",
            &format!(
                "{restore}&ifGenerationMatch={}",
                restored["generation"].as_str().unwrap()
            ),
            AUTH,
            b"",
        )
        .json_ok();
    assert_ne!(again["generation"], restored["generation"]);
    let previous = server.request(
        "GET",
        &format!(
            "{OBJECT}?generation={}&alt=media",
            restored["generation"].as_str().unwrap()
        ),
        AUTH,
        b"",
    );
    assert_eq!(previous.body, b"retained body");
    let live_restore = format!(
        "{OBJECT}/restore?generation={}",
        again["generation"].as_str().unwrap()
    );
    server
        .request("POST", &live_restore, AUTH, b"")
        .json_reason(412, "objectNotSoftDeleted");
    assert!(!data.exists());
    server.stop();
    let mut fresh = Server::start(&data, &[]);
    fresh
        .request("GET", "/storage/v1/b/test-bucket", "", b"")
        .json_reason(404, "notFound");
    fresh.stop();
    assert!(!data.exists());
}

#[test]
fn memory_resumable_wire_keeps_capability_ranges_hashes_replay_and_restart_isolation() {
    let dir = common::TestDir::new();
    let data = dir.0.join("resumable-not-created");
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}}
        ]}),
    );
    let options = [
        "--auth-mode",
        "strict",
        "--identity-file",
        identities.to_str().unwrap(),
    ];
    let mut server = Server::start(&data, &options);
    server.bucket(true);
    let original = server.put(b"old bytes").json_ok();
    let path = |reply: &Reply| {
        reply.expect(200);
        let uri: hyper::Uri = reply.headers["location"].parse().unwrap();
        uri.path_and_query().unwrap().as_str().to_owned()
    };
    let session = path(&server.request("POST", &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=value&ifGenerationMatch={}", original["generation"].as_str().unwrap()),
        AUTH, br#"{"contentType":"application/test","metadata":{"origin":"ram"}}"#));
    let prefix = vec![b'p'; 256 * 1024];
    let first = server.request(
        "POST",
        &session,
        "Content-Range: bytes 0-262143/*\r\nX-GUploader-No-308: yes\r\n",
        &prefix,
    );
    first.expect(200);
    assert_eq!(first.headers["x-http-status-code-override"], "308");
    assert_eq!(first.headers["range"], "bytes=0-262143");
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        original["generation"]
    );
    let status = server.request("PUT", &session, "Content-Range: bytes */*\r\n", b"");
    status.expect(308);
    assert_eq!(status.headers["range"], first.headers["range"]);
    let replay = server.request(
        "PUT",
        &session,
        "Content-Range: bytes 0-262143/*\r\n",
        &vec![b'x'; prefix.len()],
    );
    replay.expect(308);
    assert_eq!(replay.headers["range"], first.headers["range"]);
    server
        .request(
            "PUT",
            &session,
            "Content-Range: bytes 262145-262148/*\r\n",
            b"gap!",
        )
        .expect(400);
    let mut expected = prefix;
    expected.extend_from_slice(b"tailRAM!");
    let reference = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=reference",
            AUTH,
            &expected,
        )
        .json_ok();
    let crc = reference["crc32c"].as_str().unwrap().to_owned();
    let headers = format!(
        "Content-Range: bytes 262144-262151/262152\r\nX-Goog-Hash: crc32c={crc}\r\nX-Goog-Meta-final: yes\r\n"
    );
    let completed = server
        .request("PUT", &session, &headers, b"tailRAM!")
        .json_ok();
    assert_eq!(completed["crc32c"], crc);
    assert_eq!(completed["metadata"], json!({"origin":"ram","final":"yes"}));
    assert_eq!(completed["contentType"], "application/test");
    assert_ne!(completed["generation"], original["generation"]);
    assert_eq!(
        server
            .request("PUT", &session, &headers, b"tailRAM!")
            .json_ok(),
        completed
    );
    assert_eq!(
        server
            .request("PUT", &session, "Content-Range: bytes */262152\r\n", b"")
            .json_ok(),
        completed
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), AUTH, b"")
            .body,
        expected
    );
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "{OBJECT}?alt=media&generation={}",
                    original["generation"].as_str().unwrap()
                ),
                AUTH,
                b""
            )
            .body,
        b"old bytes"
    );
    let invalid = path(&server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=value",
        AUTH,
        b"",
    ));
    server
        .request("PUT", &invalid, "X-Goog-Hash: crc32c=AAAAAA==\r\n", b"bad")
        .expect(400);
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        completed["generation"]
    );
    server
        .request("PUT", &invalid, "Content-Range: bytes */*\r\n", b"")
        .expect(410);
    let cancelled = path(&server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=cancelled",
        AUTH,
        b"",
    ));
    server.request("DELETE", &cancelled, "", b"").expect(499);
    server
        .request("PUT", &cancelled, "Content-Range: bytes */*\r\n", b"")
        .expect(499);
    assert!(!data.exists());
    server.stop();
    let mut restarted = Server::start(&data, &options);
    restarted
        .request("PUT", &session, "Content-Range: bytes */*\r\n", b"")
        .expect(404);
    restarted.stop();
    assert!(!data.exists());
}

#[test]
fn memory_xml_multipart_wire_replaces_parts_completes_and_cleans_without_disk_paths() {
    let dir = common::TestDir::new();
    let data = dir.0.join("multipart-not-created");
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}}
        ]}),
    );
    let mut server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    server.bucket(true);
    let original = server.put(b"old bytes").json_ok();
    let begin = || {
        let reply = server.request(
            "POST",
            "/test-bucket/value?uploads",
            &format!("{AUTH}x-goog-meta-kind: ram-multipart\r\n"),
            b"",
        );
        reply.expect(200);
        let body = String::from_utf8(reply.body).unwrap();
        body.split_once("<UploadId>")
            .unwrap()
            .1
            .split_once("</UploadId>")
            .unwrap()
            .0
            .to_owned()
    };
    let id = begin();
    let target = format!("/test-bucket/value?uploadId={id}");
    server.request("GET", &target, "", b"").expect(401);
    let large = vec![b'a'; 5 * 1024 * 1024];
    let first = server.request("PUT", &format!("{target}&partNumber=1"), AUTH, &large);
    first.expect(200);
    let old_etag = first.headers["etag"].clone();
    server
        .request(
            "PUT",
            &format!("{target}&partNumber=1"),
            &format!("{AUTH}Content-MD5: 1B2M2Y8AsgTpgAmY7PhCfg==\r\n"),
            b"wrong",
        )
        .expect(400);
    let mut expected = vec![b'b'; large.len()];
    let replaced = server.request("PUT", &format!("{target}&partNumber=1"), AUTH, &expected);
    replaced.expect(200);
    let etag = replaced.headers["etag"].clone();
    assert_ne!(etag, old_etag);
    let tail = server.request("PUT", &format!("{target}&partNumber=7"), AUTH, b"tail");
    tail.expect(200);
    let tail_etag = &tail.headers["etag"];
    let parts = server.request("GET", &format!("{target}&max-parts=1"), AUTH, b"");
    parts.expect(200);
    let parts = String::from_utf8(parts.body).unwrap();
    assert!(parts.contains("<IsTruncated>true</IsTruncated>"));
    assert!(parts.contains("<NextPartNumberMarker>1</NextPartNumberMarker>"));
    let uploads = server.request("GET", "/test-bucket?uploads", AUTH, b"");
    uploads.expect(200);
    assert!(String::from_utf8(uploads.body).unwrap().contains(&id));
    let manifest = |first: &str| {
        format!(
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{first}</ETag></Part><Part><PartNumber>7</PartNumber><ETag>{tail_etag}</ETag></Part></CompleteMultipartUpload>"
        )
    };
    server
        .request("POST", &target, AUTH, manifest(&old_etag).as_bytes())
        .expect(400);
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        original["generation"]
    );
    let completed = server.request("POST", &target, AUTH, manifest(&etag).as_bytes());
    completed.expect(200);
    let metadata = server.request("GET", OBJECT, AUTH, b"").json_ok();
    assert!(metadata.get("md5Hash").is_none());
    assert!(metadata.get("componentCount").is_none());
    assert_eq!(metadata["metadata"]["kind"], "ram-multipart");
    expected.extend_from_slice(b"tail");
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), AUTH, b"")
            .body,
        expected
    );
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "{OBJECT}?alt=media&generation={}",
                    original["generation"].as_str().unwrap()
                ),
                AUTH,
                b""
            )
            .body,
        b"old bytes"
    );
    server
        .request("POST", &target, AUTH, manifest(&etag).as_bytes())
        .expect(404);
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        metadata["generation"]
    );
    let copied = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/value/copyTo/b/test-bucket/o/multipart-copy",
            AUTH,
            b"",
        )
        .json_ok();
    assert!(copied.get("md5Hash").is_some());
    assert_eq!(copied["crc32c"], metadata["crc32c"]);
    let cancelled = begin();
    let cancelled = format!("/test-bucket/value?uploadId={cancelled}");
    server
        .request("PUT", &format!("{cancelled}&partNumber=1"), AUTH, b"unused")
        .expect(200);
    server.request("DELETE", &cancelled, AUTH, b"").expect(204);
    server.request("GET", &cancelled, AUTH, b"").expect(404);
    assert_eq!(
        server.request("GET", OBJECT, AUTH, b"").json_ok()["generation"],
        metadata["generation"]
    );
    assert!(!data.exists());
    server.stop();
    assert!(!data.exists());
}

#[test]
fn memory_seed_streams_read_only_fixtures_without_a_disk_catalog() {
    let dir = common::TestDir::new();
    let seed = dir.0.join("seed");
    fs::create_dir_all(seed.join("test-bucket/nested")).unwrap();
    fs::write(seed.join("test-bucket/nested/value"), b"seed bytes").unwrap();
    let data = dir.0.join("never-created");
    let mut server = Server::start(&data, &["--seed-dir", seed.to_str().unwrap()]);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/nested%2Fvalue?alt=media",
                "",
                b""
            )
            .body,
        b"seed bytes"
    );
    server.stop();
    assert_eq!(
        fs::read(seed.join("test-bucket/nested/value")).unwrap(),
        b"seed bytes"
    );
    assert!(!data.exists());
}

const SMALL_BUDGET: [&str; 2] = ["--memory-max-bytes", "260"];

#[test]
fn memory_backend_default_frees_deleted_data_so_repeated_create_delete_never_exhausts_capacity() {
    let dir = common::TestDir::new();
    let data = dir.0.join("never-created");
    let mut server = Server::start_with(&data, &SMALL_BUDGET, SoftDeleteEnv::Unset);
    server.bucket(false);
    for round in 0..8 {
        let put = server.put(b"body");
        put.expect_with(
            200,
            format_args!("round {round}: {}", String::from_utf8_lossy(&put.body)),
        );
        server
            .request("DELETE", OBJECT, AUTH, b"")
            .expect_with(204, format_args!("round {round}"));
    }
    let bucket = server
        .request("GET", "/storage/v1/b/test-bucket", AUTH, b"")
        .json_ok();
    assert_ne!(
        bucket["softDeletePolicy"]["retentionDurationSeconds"],
        "604800"
    );
    server.stop();
}

#[test]
fn memory_backend_keeps_seven_day_soft_delete_when_asked_and_capacity_errors_name_the_cause() {
    let dir = common::TestDir::new();
    let data = dir.0.join("never-created");
    let mut options = SMALL_BUDGET.to_vec();
    options.extend(["--default-soft-delete-retention-seconds", "604800"]);
    let mut server = Server::start_with(&data, &options, SoftDeleteEnv::Unset);
    server.bucket(false);
    let bucket = server
        .request("GET", "/storage/v1/b/test-bucket", AUTH, b"")
        .json_ok();
    assert_eq!(
        bucket["softDeletePolicy"]["retentionDurationSeconds"],
        "604800"
    );
    let mut exhausted = None;
    for _ in 0..8 {
        let put = server.put(b"body");
        if put.status == 503 {
            exhausted = Some(put);
            break;
        }
        put.expect(200);
        server.request("DELETE", OBJECT, AUTH, b"").expect(204);
    }
    let exhausted = exhausted.expect("retained soft-deleted data must exhaust the small budget");
    exhausted.json_reason(503, "backendError");
    let message: Value = serde_json::from_slice(&exhausted.body).unwrap();
    let message = message["error"]["message"].as_str().unwrap();
    for needle in [
        "soft delete",
        "default_soft_delete_retention_seconds",
        "softDeletePolicy",
    ] {
        assert!(message.contains(needle), "{needle}: {message}");
    }
    server.stop();
}
