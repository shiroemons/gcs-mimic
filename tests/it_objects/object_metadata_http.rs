use crate::common;
use crate::common::process::declare_server;

use common::TestDir;
use common::process::http_command;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, Barrier},
    time::Duration,
};

use common::http::{Client, Reply};

impl Server {
    fn start(dir: &TestDir) -> Self {
        Self::launch(http_command(&dir.0).args(["--api-base-url", "http://127.0.0.1:4443"]))
    }
    fn request(&self, method: &str, path: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, body)
    }
    fn setup(&self) -> Value {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            br#"{"name":"test-bucket"}"#,
        )
        .expect(200);
        self.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
            b"payload",
        )
        .json_ok()
    }
    fn update(&self, method: &str, query: &str, body: Value) -> Reply {
        self.request(
            method,
            &format!("/storage/v1/b/test-bucket/o/dir%2Ffile{query}"),
            body.to_string().as_bytes(),
        )
    }
    fn stop(&mut self) {
        self.0.stop();
    }
}
fn request(address: &str, method: &str, path: &str, body: &[u8]) -> Reply {
    request_headers(address, method, path, body, "")
}

fn request_headers(address: &str, method: &str, path: &str, body: &[u8], headers: &str) -> Reply {
    Client {
        address,
        host: "localhost",
        headers: "Content-Type: application/json\r\n",
        timeout: Duration::from_secs(5),
    }
    .request_eof(method, path, headers, body)
    .into_last()
}

#[test]
fn null_generation_metadata_updates_preserve_identity_and_enforce_conditions() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let original = server.setup();
    let path = "/storage/v1/b/test-bucket/o/dir%2Ffile";
    for (method, headers) in [
        ("PATCH", ""),
        ("PUT", ""),
        ("POST", "X-HTTP-Method-Override: PATCH\r\n"),
    ] {
        let send = |query: &str, body: Value| {
            request_headers(
                &server.address,
                method,
                &format!("{path}?projection=full{query}"),
                body.to_string().as_bytes(),
                headers,
            )
        };
        let body = json!({"bucket":"test-bucket", "name":"dir/file", "generation":null,
            "metadata":{"fixture":method}});
        let before = server.request("GET", path, b"").json_ok();
        let updated = send("", body).json_ok();
        assert_eq!(updated["metadata"], json!({"fixture":method}));
        assert_eq!(
            updated["metageneration"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap(),
            before["metageneration"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                + 1
        );
        for field in [
            "generation",
            "size",
            "md5Hash",
            "crc32c",
            "timeCreated",
            "timeFinalized",
        ] {
            assert_eq!(updated[field], original[field], "{method}: {field}");
        }
        let rejected = json!({"generation":null, "metadata":{"fixture":"rejected"}});
        for (query, status, reason) in [
            ("&generation=0", 404, "notFound"),
            ("&ifGenerationMatch=0", 412, "conditionNotMet"),
            ("&ifMetagenerationMatch=1", 412, "conditionNotMet"),
        ] {
            send(query, rejected.clone()).metadata_error(status, reason);
            assert_eq!(
                server
                    .request("GET", &format!("{path}?projection=full"), b"")
                    .json_ok(),
                updated
            );
        }
        for generation in [json!(0), json!("0"), json!(true), json!([])] {
            send(
                "",
                json!({"generation":generation, "metadata":{"fixture":"rejected"}}),
            )
            .metadata_error(400, "badRequest");
            assert_eq!(
                server
                    .request("GET", &format!("{path}?projection=full"), b"")
                    .json_ok(),
                updated
            );
        }
        let media = server.request("GET", &format!("{path}?alt=media"), b"");
        media.expect(200);
        assert_eq!(media.body, b"payload");
    }
    server.stop();
}

#[test]
fn patch_merges_put_replaces_and_download_reflects_persistent_metadata() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let original = server.setup();
    let initial = json!({"contentType":"text/plain", "contentEncoding":"br", "contentDisposition":"attachment; filename=hello.txt", "contentLanguage":"en", "cacheControl":"private, max-age=60", "metadata":{"keep":"one","drop":"two"}});
    let first = server
        .update("PATCH", "?projection=full", initial)
        .json_ok();
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
    let identical = server.update("PUT", "", first).json_ok();
    assert_eq!(identical["metageneration"], "3");
    assert_eq!(
        server
            .update("PATCH", "", json!({"contentType":""}))
            .json_ok()["contentType"],
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
            .json_ok();
        server
            .update("PATCH", "", json!({key:value[key]}))
            .expect_with(200, format_args!("{key}"));
        server
            .update("PATCH", "", json!({key:"changed"}))
            .metadata_error(400, "badRequest");
    }
    let second = server
        .update("PATCH", "", json!({"metadata":{"drop":null,"add":"three"}}))
        .json_ok();
    assert_eq!(second["metadata"], json!({"keep":"one","add":"three"}));
    assert_eq!(second["contentType"], "text/plain");
    let media = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/dir%2Ffile?alt=media",
        b"",
    );
    media.expect(200);
    assert_eq!(media.body, b"payload");
    for (key, value) in [
        ("content-type", "text/plain"),
        ("content-encoding", "br"),
        ("content-disposition", "attachment; filename=hello.txt"),
        ("content-language", "en"),
        (
            "cache-control",
            "no-cache, no-store, max-age=0, must-revalidate",
        ),
        ("x-goog-stored-content-encoding", "br"),
    ] {
        assert_eq!(media.headers[key], value);
    }
    server.stop();
    let mut server = Server::start(&dir);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
            .json_ok(),
        second
    );
    let replaced = server
        .update(
            "PUT",
            "?projection=noAcl",
            json!({"metadata":{"only":"new"}}),
        )
        .json_ok();
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
        .json_ok();
    assert!(cleared.get("metadata").is_none());
    assert!(cleared.get("contentType").is_none());
    let empty_value = server
        .update("PATCH", "", json!({"metadata":{"empty":""}}))
        .json_ok();
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
    server
        .update(
            "PATCH",
            "",
            json!({"generation":generation,"metageneration":1,"size":7}),
        )
        .expect(200);
    for method in ["PATCH", "PUT"] {
        server
            .update(method, "", json!({"contentLanguage":""}))
            .metadata_error(400, "invalidArgument");
        server
            .request(
                method,
                "/storage/v1/b/test-bucket/o/dir%2Ffile",
                &vec![b' '; 16 * 1024 * 1024 + 1],
            )
            .metadata_error(413, "uploadTooLarge");
        for (key, equal_succeeds) in [
            ("ifGenerationMatch", true),
            ("ifGenerationNotMatch", false),
            ("ifMetagenerationMatch", true),
            ("ifMetagenerationNotMatch", false),
        ] {
            for success in [false, true] {
                let current = server
                    .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
                    .json_ok();
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
                    assert_eq!(reply.json_ok()["generation"], original["generation"]);
                } else {
                    reply.metadata_error(412, "conditionNotMet");
                    assert_eq!(
                        server
                            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
                            .json_ok(),
                        current
                    );
                }
            }
        }
        server
            .update(method, "?generation=0", json!({}))
            .metadata_error(404, "notFound");
        server
            .update(method, &format!("?generation={generation}"), json!({}))
            .expect(200);
        server
            .update(method, "?ifGenerationMatch=0", json!({}))
            .metadata_error(412, "conditionNotMet");
        server
            .update(method, "?ifGenerationNotMatch=0", json!({}))
            .expect(200);
    }
    for method in ["PATCH", "PUT"] {
        let before = server
            .update(
                "PATCH",
                "",
                json!({"cacheControl":"private", "metadata":{"old":"value"}}),
            )
            .json_ok();
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
            .json_ok();
        assert_eq!(
            after,
            common::without_acl(
                replies
                    .iter()
                    .find(|reply| reply.status == 200)
                    .unwrap()
                    .json_ok()
            )
        );
        replies
            .iter()
            .find(|reply| reply.status == 412)
            .unwrap()
            .metadata_error(412, "conditionNotMet");
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
        let before = server.request("GET", path, b"").json_ok();
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
                patch.metadata_error(404, "notFound");
                assert!(delete.body.is_empty());
                server
                    .request("GET", path, b"")
                    .metadata_error(404, "notFound");
                server
                    .request("GET", &format!("{path}?alt=media"), b"")
                    .metadata_error(404, "notFound");
                let listing = server
                    .request("GET", "/storage/v1/b/test-bucket/o", b"")
                    .json_ok();
                assert!(listing.get("items").is_none());
                server
                    .request(
                        "POST",
                        "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
                        b"payload",
                    )
                    .json_ok();
            }
            (412, 200) => {
                delete.metadata_error(412, "conditionNotMet");
                let after = server.request("GET", path, b"").json_ok();
                assert_eq!(after, patch.json_ok());
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
        assert_eq!(common::process::staging_count(&dir), 0);
        assert_eq!(common::process::blob_count(&dir), 1);
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
            .metadata_error(400, "badRequest");
        server
            .update(method, "", json!({"cacheControl":"private\tmax-age=0"}))
            .metadata_error(400, "invalid");
        for (body, reason) in [
            ("{", "invalid"),
            ("[]", "invalidArgument"),
            ("null", "invalidArgument"),
            (r#"{"contentType":7}"#, "invalidArgument"),
            (r#"{"customTime":"invalid"}"#, "invalid"),
            (r#"{"contentLanguage":"en\r\nx-bad: true"}"#, "invalid"),
        ] {
            server
                .request(
                    method,
                    "/storage/v1/b/test-bucket/o/dir%2Ffile",
                    body.as_bytes(),
                )
                .metadata_error(400, reason);
        }
        for field in [
            "predefinedAcl",
            "contexts",
            "storageClass",
            "kmsKeyName",
            "customerEncryption",
            "name",
            "bucket",
            "metageneration",
            "size",
            "md5Hash",
            "crc32c",
            "timeCreated",
            "updated",
            "etag",
        ] {
            server
                .update(method, "", json!({field:null}))
                .metadata_error(400, "badRequest");
        }
        server
            .update(method, "", json!({"acl":null}))
            .metadata_error(400, "invalid");
        for query in ["predefinedAcl=private", "userProject=test"] {
            server
                .update(method, &format!("?{query}"), json!({}))
                .metadata_error(400, "badRequest");
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
                .metadata_error(400, "invalidParameter");
        }
        for path in [
            "/storage/v1/b/absent-bucket/o/file",
            "/storage/v1/b/test-bucket/o/absent",
        ] {
            server
                .request(method, path, b"{}")
                .metadata_error(404, "notFound");
        }
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/dir%2Ffile", b"")
            .json_ok(),
        original
    );
    server
        .update("PATCH", "", json!({"metadata":{"a":"x".repeat(8191)}}))
        .expect(200);
    server
        .update("PATCH", "", json!({"metadata":{"b":""}}))
        .metadata_error(400, "invalidArgument");
    server
        .update(
            "PATCH",
            "",
            json!({"metadata":{"a":null,"b":"x".repeat(8191)}}),
        )
        .expect(200);
    server
        .update("PUT", "", json!({"metadata":{"雪":"x".repeat(8190)}}))
        .metadata_error(400, "invalidArgument");
    server
        .update("PATCH", "", json!({"contentEncoding":"gzip"}))
        .json_ok();
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"GET /storage/v1/b/test-bucket/o/dir%2Ffile?alt=media HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = Vec::new();
    if let Err(error) = stream.read_to_end(&mut response) {
        assert!(common::closed_by_server(&error), "{error}");
    }
    // A plain body declared as gzip fails before any header is sent.
    assert!(response.starts_with(b"HTTP/1.1 400"));
    assert!(response.ends_with(b"Bad Request"));
    server
        .update("PATCH", "", json!({"cacheControl":"no-transform"}))
        .json_ok();
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
    server
        .update("PATCH", "", json!({"contentLanguage":"a".repeat(100)}))
        .expect(200);
    server
        .update("PATCH", "", json!({"contentLanguage":"a".repeat(101)}))
        .metadata_error(400, "invalid");
    server
        .update(
            "PATCH",
            "",
            json!({"contentDisposition":"attachment; filename=雪"}),
        )
        .metadata_error(400, "badRequest");
    server
        .update("PATCH", "", json!({"metadata":{"":"value"}}))
        .metadata_error(400, "badRequest");
    let compressed = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03\x4b\x4c\x4a\x06\x00\xc2\x41\x24\x35\x03\x00\x00\x00";
    let uploaded = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
            compressed,
        )
        .json_ok();
    server
        .update(
            "PATCH",
            "",
            json!({"contentEncoding":"gzip","metadata":{"key":"value"}}),
        )
        .json_ok();
    for (encoding, status, expanded) in [
        ("gzip", 200, false),
        ("gzip;q=1.0", 200, false),
        ("*", 200, false),
        ("br, gzip;q=0.1", 200, false),
        ("gzip;q=0", 200, true),
        ("*;q=1,gzip;q=0", 200, true),
        ("gzip;q=0,*", 200, true),
        ("*;q=0", 200, true),
        ("gzip;q=2", 400, false),
        ("gzip;q=oops", 400, false),
    ] {
        let reply = request_headers(
            &server.address,
            "GET",
            "/storage/v1/b/test-bucket/o/dir%2Ffile?alt=media",
            b"",
            &format!("Accept-Encoding: {encoding}\r\n"),
        );
        assert_eq!(reply.status, status, "{encoding}");
        if expanded {
            assert_eq!(reply.body, b"abc");
            assert!(!reply.headers.contains_key("content-encoding"));
            assert!(!reply.headers.contains_key("content-length"));
        } else if status == 200 {
            assert_eq!(reply.body, compressed);
            assert_eq!(reply.headers["content-encoding"], "gzip");
        } else {
            reply.metadata_error(400, "invalidParameter");
        }
    }
    let replaced = server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=dir%2Ffile",
            b"new bytes",
        )
        .json_ok();
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

declare_server!();
