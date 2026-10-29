mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    sync::{Arc, Barrier},
    time::Duration,
};

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}
struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Server {
    fn start(dir: &TestDir) -> Self {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
        }
    }
    fn request(&self, method: &str, path: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, body)
    }
    fn setup(&self) -> Value {
        assert_eq!(
            self.request(
                "POST",
                "/storage/v1/b?project=local",
                br#"{"name":"test-bucket"}"#
            )
            .status,
            200
        );
        self.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
            b"payload",
        )
        .json()
    }
    fn update(&self, method: &str, query: &str, body: Value) -> Reply {
        self.request(
            method,
            &format!("/storage/v1/b/test-bucket/o/dir%2Ffile{query}"),
            body.to_string().as_bytes(),
        )
    }
    fn stop(&mut self) {
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
impl Reply {
    fn json(&self) -> Value {
        assert_eq!(self.status, 200, "{}", String::from_utf8_lossy(&self.body));
        serde_json::from_slice(&self.body).unwrap()
    }
    fn error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        let value: Value = serde_json::from_slice(&self.body).unwrap();
        assert_eq!(value["error"]["errors"][0]["reason"], reason);
    }
}
fn request(address: &str, method: &str, path: &str, body: &[u8]) -> Reply {
    request_headers(address, method, path, body, "")
}

fn request_headers(address: &str, method: &str, path: &str, body: &[u8], headers: &str) -> Reply {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(body).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut headers = BTreeMap::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    let mut body = Vec::new();
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value == "chunked")
    {
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let size = usize::from_str_radix(line.trim(), 16).unwrap();
            if size == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..]).unwrap();
            let mut crlf = [0; 2];
            reader.read_exact(&mut crlf).unwrap();
            assert_eq!(&crlf, b"\r\n");
        }
    } else {
        reader.read_to_end(&mut body).unwrap();
    }
    Reply {
        status,
        headers,
        body,
    }
}

#[test]
fn patch_merges_put_replaces_and_download_reflects_persistent_metadata() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let original = server.setup();
    let initial = json!({"contentType":"text/plain", "contentEncoding":"br", "contentDisposition":"attachment; filename=hello.txt", "contentLanguage":"en", "cacheControl":"private, max-age=60", "metadata":{"keep":"one","drop":"two"}});
    let first = server.update("PATCH", "?projection=full", initial).json();
    assert_eq!(first["metageneration"], "2");
    for field in [
        "generation",
        "size",
        "md5Hash",
        "crc32c",
        "timeCreated",
        "timeFinalized",
    ] {
        assert_eq!(first[field], original[field], "{field}");
    }
    assert_ne!(first["etag"], original["etag"]);
    let identical = server.update("PUT", "", first.clone()).json();
    assert_eq!(identical["metageneration"], "3");
    assert_eq!(
        server.update("PATCH", "", json!({"contentType":""})).json()["contentType"],
        "text/plain"
    );
    for key in [
        "name",
        "bucket",
        "generation",
        "metageneration",
        "size",
        "md5Hash",
        "crc32c",
        "storageClass",
        "timeCreated",
        "updated",
        "etag",
        "kind",
        "id",
        "selfLink",
        "mediaLink",
        "timeFinalized",
        "timeStorageClassUpdated",
    ] {
        let value = server
            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
            .json();
        assert_eq!(
            server.update("PATCH", "", json!({key:value[key]})).status,
            200,
            "{key}"
        );
        server
            .update("PATCH", "", json!({key:"changed"}))
            .error(400, "badRequest");
    }
    let second = server
        .update("PATCH", "", json!({"metadata":{"drop":null,"add":"three"}}))
        .json();
    assert_eq!(second["metadata"], json!({"keep":"one","add":"three"}));
    assert_eq!(second["contentType"], "text/plain");
    let media = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/dir%2Ffile?alt=media",
        b"",
    );
    assert_eq!(media.status, 200);
    assert_eq!(media.body, b"payload");
    for (key, value) in [
        ("content-type", "text/plain"),
        ("content-encoding", "br"),
        ("content-disposition", "attachment; filename=hello.txt"),
        ("content-language", "en"),
        ("cache-control", "no-cache"),
        ("x-goog-stored-content-encoding", "br"),
    ] {
        assert_eq!(media.headers[key], value);
    }
    server.stop();
    let mut server = Server::start(&dir);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
            .json(),
        second
    );
    let replaced = server
        .update(
            "PUT",
            "?projection=noAcl",
            json!({"metadata":{"only":"new"}}),
        )
        .json();
    assert!(replaced.get("contentType").is_none());
    assert_eq!(replaced["metadata"], json!({"only":"new"}));
    for key in [
        "contentEncoding",
        "contentDisposition",
        "contentLanguage",
        "cacheControl",
    ] {
        assert!(replaced.get(key).is_none());
    }
    assert_eq!(
        replaced["metageneration"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap(),
        second["metageneration"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
            + 1
    );
    let cleared = server
        .update("PATCH", "", json!({"metadata":null,"contentType":null}))
        .json();
    assert!(cleared.get("metadata").is_none());
    assert!(cleared.get("contentType").is_none());
    let empty_value = server
        .update("PATCH", "", json!({"metadata":{"empty":""}}))
        .json();
    assert_eq!(empty_value["metadata"], json!({"empty":""}));
    let media = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/dir%2Ffile?alt=media",
        b"",
    );
    assert_eq!(media.body, b"payload");
    assert_eq!(media.headers["content-type"], "application/octet-stream");
    assert!(!media.headers.contains_key("content-encoding"));
    server.stop();
}

#[test]
fn all_conditions_generation_selectors_and_conditioned_races() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let original = server.setup();
    let generation = original["generation"]
        .as_str()
        .unwrap()
        .parse::<i64>()
        .unwrap();
    assert_eq!(
        server
            .update(
                "PATCH",
                "",
                json!({"generation":generation,"metageneration":1,"size":7})
            )
            .status,
        200
    );
    for method in ["PATCH", "PUT"] {
        server
            .update(method, "", json!({"contentLanguage":""}))
            .error(400, "invalidArgument");
        server
            .request(
                method,
                "/storage/v1/b/test-bucket/o/dir%2Ffile",
                &vec![b' '; 65537],
            )
            .error(413, "uploadTooLarge");
        for (key, equal_succeeds) in [
            ("ifGenerationMatch", true),
            ("ifGenerationNotMatch", false),
            ("ifMetagenerationMatch", true),
            ("ifMetagenerationNotMatch", false),
        ] {
            for success in [false, true] {
                let current = server
                    .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
                    .json();
                let actual = if key.contains("Metageneration") {
                    current["metageneration"]
                        .as_str()
                        .unwrap()
                        .parse::<i64>()
                        .unwrap()
                } else {
                    generation
                };
                let value = actual + i64::from(success != equal_succeeds);
                let reply = server.update(method, &format!("?{key}={value}"), json!({}));
                if success {
                    assert_eq!(reply.json()["generation"], original["generation"]);
                } else {
                    reply.error(412, "conditionNotMet");
                    assert_eq!(
                        server
                            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
                            .json(),
                        current
                    );
                }
            }
        }
        server
            .update(method, "?generation=0", json!({}))
            .error(404, "notFound");
        assert_eq!(
            server
                .update(method, &format!("?generation={generation}"), json!({}))
                .status,
            200
        );
        server
            .update(method, "?ifGenerationMatch=0", json!({}))
            .error(412, "conditionNotMet");
        assert_eq!(
            server
                .update(method, "?ifGenerationNotMatch=0", json!({}))
                .status,
            200
        );
    }
    for method in ["PATCH", "PUT"] {
        let before = server
            .update(
                "PATCH",
                "",
                json!({"cacheControl":"private", "metadata":{"old":"value"}}),
            )
            .json();
        let barrier = Arc::new(Barrier::new(3));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let address = server.address.clone();
                let barrier = barrier.clone();
                let path = format!(
                    "/storage/v1/b/test-bucket/o/dir%2Ffile?ifMetagenerationMatch={}",
                    before["metageneration"].as_str().unwrap()
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    request(&address, method, &path, br#"{"metadata":{"winner":"yes"}}"#)
                })
            })
            .collect();
        barrier.wait();
        let replies: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        let mut statuses: Vec<_> = replies.iter().map(|reply| reply.status).collect();
        statuses.sort();
        assert_eq!(statuses, [200, 412]);
        let after = server
            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
            .json();
        assert_eq!(
            after,
            common::without_acl(
                replies
                    .iter()
                    .find(|reply| reply.status == 200)
                    .unwrap()
                    .json()
            )
        );
        replies
            .iter()
            .find(|reply| reply.status == 412)
            .unwrap()
            .error(412, "conditionNotMet");
        if method == "PUT" {
            assert_eq!(after["metadata"], json!({"winner":"yes"}));
            assert!(after.get("cacheControl").is_none());
        } else {
            assert_eq!(after["metadata"], json!({"old":"value", "winner":"yes"}));
            assert_eq!(after["cacheControl"], "private");
        }
        assert_eq!(
            after["metageneration"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap(),
            before["metageneration"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()
                + 1
        );
    }
    server.stop();
}

#[test]
fn conditional_delete_and_metadata_patch_have_one_winner() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    server.setup();
    let path = "/storage/v1/b/test-bucket/o/dir%2Ffile";
    for attempt in 0..12 {
        let before = server.request("GET", path, b"").json();
        let condition = format!(
            "{path}?ifMetagenerationMatch={}",
            before["metageneration"].as_str().unwrap()
        );
        let barrier = Arc::new(Barrier::new(2));
        let address = server.address.clone();
        let waiting = barrier.clone();
        let worker_path = condition.clone();
        let (worker_method, main_method) = if attempt % 2 == 0 {
            ("PATCH", "DELETE")
        } else {
            ("DELETE", "PATCH")
        };
        let patch_body = br#"{"metadata":{"winner":"patch"}}"#;
        let worker = std::thread::spawn(move || {
            waiting.wait();
            request(
                &address,
                worker_method,
                &worker_path,
                if worker_method == "PATCH" {
                    patch_body
                } else {
                    b""
                },
            )
        });
        barrier.wait();
        let main = server.request(
            main_method,
            &condition,
            if main_method == "PATCH" {
                patch_body
            } else {
                b""
            },
        );
        let worker = worker.join().unwrap();
        let (delete, patch) = if main_method == "DELETE" {
            (main, worker)
        } else {
            (worker, main)
        };
        match (delete.status, patch.status) {
            (204, 404) => {
                patch.error(404, "notFound");
                assert!(delete.body.is_empty());
                server.request("GET", path, b"").error(404, "notFound");
                server
                    .request("GET", &format!("{path}?alt=media"), b"")
                    .error(404, "notFound");
                let listing = server
                    .request("GET", "/storage/v1/b/test-bucket/o", b"")
                    .json();
                assert!(listing.get("items").is_none());
                server
                    .request(
                        "POST",
                        "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
                        b"payload",
                    )
                    .json();
            }
            (412, 200) => {
                delete.error(412, "conditionNotMet");
                let after = server.request("GET", path, b"").json();
                assert_eq!(after, patch.json());
                assert_eq!(after["metadata"], json!({"winner":"patch"}));
                assert_eq!(after["generation"], before["generation"]);
                assert_eq!(
                    after["metageneration"]
                        .as_str()
                        .unwrap()
                        .parse::<i64>()
                        .unwrap(),
                    before["metageneration"]
                        .as_str()
                        .unwrap()
                        .parse::<i64>()
                        .unwrap()
                        + 1
                );
                assert_eq!(
                    server
                        .request("GET", &format!("{path}?alt=media"), b"")
                        .body,
                    b"payload"
                );
            }
            statuses => panic!("exactly one mutation must succeed: {statuses:?}"),
        }
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(dir.0.join("blobs"))
                .unwrap()
                .map(|shard| std::fs::read_dir(shard.unwrap().path()).unwrap().count())
                .sum::<usize>(),
            1
        );
    }
    server.stop();
}

#[test]
fn invalid_and_unsupported_metadata_never_mutates_objects() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let original = server.setup();
    for method in ["PATCH", "PUT"] {
        server
            .update(
                method,
                "",
                json!({"contentDisposition":"filename=日本語.txt"}),
            )
            .error(400, "badRequest");
        server
            .update(method, "", json!({"cacheControl":"private\tmax-age=0"}))
            .error(400, "invalidArgument");
        for (body, reason) in [
            ("{", "parseError"),
            ("[]", "invalidArgument"),
            ("null", "invalidArgument"),
            (r#"{"contentType":7}"#, "invalidArgument"),
            (r#"{"eventBasedHold":null}"#, "invalidArgument"),
            (r#"{"temporaryHold":"true"}"#, "invalidArgument"),
            (r#"{"customTime":"invalid"}"#, "invalidArgument"),
            (r#"{"metadata":[]}"#, "invalidArgument"),
            (r#"{"metadata":{"a":false}}"#, "invalidArgument"),
            (
                r#"{"contentLanguage":"en\r\nx-bad: true"}"#,
                "invalidArgument",
            ),
        ] {
            server
                .request(
                    method,
                    "/storage/v1/b/test-bucket/o/dir%2Ffile",
                    body.as_bytes(),
                )
                .error(400, reason);
        }
        for field in [
            "predefinedAcl",
            "contexts",
            "storageClass",
            "kmsKeyName",
            "customerEncryption",
            "name",
            "bucket",
            "generation",
            "metageneration",
            "size",
            "md5Hash",
            "crc32c",
            "timeCreated",
            "updated",
            "etag",
            "unknownFutureField",
        ] {
            server
                .update(method, "", json!({field:null}))
                .error(400, "badRequest");
        }
        server
            .update(method, "", json!({"acl":null}))
            .error(400, "invalidArgument");
        for query in ["predefinedAcl=private", "userProject=test"] {
            server
                .update(method, &format!("?{query}"), json!({}))
                .error(400, "badRequest");
        }
        for query in [
            "generation=x",
            "ifGenerationMatch=9223372036854775808",
            "projection=invalid",
            "generation=1&generation=2",
            "overrideUnlockedRetention=yes",
        ] {
            server
                .update(method, &format!("?{query}"), json!({}))
                .error(400, "invalidParameter");
        }
        for path in [
            "/storage/v1/b/absent-bucket/o/file",
            "/storage/v1/b/test-bucket/o/absent",
        ] {
            server.request(method, path, b"{}").error(404, "notFound");
        }
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
            .json(),
        original
    );
    assert_eq!(
        server
            .update("PATCH", "", json!({"metadata":{"a":"x".repeat(8191)}}))
            .status,
        200
    );
    server
        .update("PATCH", "", json!({"metadata":{"b":""}}))
        .error(400, "invalidArgument");
    assert_eq!(
        server
            .update(
                "PATCH",
                "",
                json!({"metadata":{"a":null,"b":"x".repeat(8191)}})
            )
            .status,
        200
    );
    server
        .update("PUT", "", json!({"metadata":{"雪":"x".repeat(8190)}}))
        .error(400, "invalidArgument");
    server
        .update("PATCH", "", json!({"contentEncoding":"gzip"}))
        .json();
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/dir%2Ffile?alt=media",
            b"",
        )
        .error(400, "badRequest");
    server
        .update("PATCH", "", json!({"cacheControl":"no-transform"}))
        .json();
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/dir%2Ffile?alt=media",
                b""
            )
            .body,
        b"payload"
    );
    server.stop();
}

#[test]
fn metadata_boundaries_encoding_negotiation_and_reupload_defaults() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    server.setup();
    assert_eq!(
        server
            .update("PATCH", "", json!({"contentLanguage":"a".repeat(100)}))
            .status,
        200
    );
    server
        .update("PATCH", "", json!({"contentLanguage":"a".repeat(101)}))
        .error(400, "invalidArgument");
    server
        .update(
            "PATCH",
            "",
            json!({"contentDisposition":"attachment; filename=雪"}),
        )
        .error(400, "badRequest");
    server
        .update("PATCH", "", json!({"metadata":{"":"value"}}))
        .error(400, "badRequest");
    let compressed = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\x4b\x4c\x4a\x06\x00\xc2\x41\x24\x35\x03\x00\x00\x00";
    let uploaded = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
            compressed,
        )
        .json();
    server
        .update(
            "PATCH",
            "",
            json!({"contentEncoding":"gzip","metadata":{"key":"value"}}),
        )
        .json();
    for (encoding, status) in [
        ("gzip", 200),
        ("gzip;q=1.0", 200),
        ("*", 200),
        ("br, gzip;q=0.1", 200),
        ("gzip;q=0", 400),
        ("*;q=1,gzip;q=0", 400),
        ("gzip;q=0,*", 400),
        ("*;q=0", 400),
        ("gzip;q=2", 400),
        ("gzip;q=oops", 400),
    ] {
        let reply = request_headers(
            &server.address,
            "GET",
            "/storage/v1/b/test-bucket/o/dir%2Ffile?alt=media",
            b"",
            &format!("Accept-Encoding: {encoding}\r\n"),
        );
        assert_eq!(reply.status, status, "{encoding}");
        if status == 200 {
            assert_eq!(reply.body, compressed);
            assert_eq!(reply.headers["content-encoding"], "gzip");
        }
    }
    let replaced = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
            b"new bytes",
        )
        .json();
    assert_ne!(replaced["generation"], uploaded["generation"]);
    assert_eq!(replaced["metageneration"], "1");
    for key in [
        "contentEncoding",
        "contentDisposition",
        "contentLanguage",
        "cacheControl",
        "metadata",
    ] {
        assert!(replaced.get(key).is_none(), "{key}");
    }
    server.stop();
}
