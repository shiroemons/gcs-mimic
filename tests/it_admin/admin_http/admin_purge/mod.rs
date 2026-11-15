use super::*;

const PURGE: &str = "/_internal/buckets/test-bucket";
const OTHER_OBJECT: &str = "/storage/v1/b/other-bucket/o/kept";

impl Server {
    fn purge(&self, path: &str, headers: &str) -> Reply {
        self.request("DELETE", path, headers, b"")
    }

    fn populate_other(&self) {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"other-bucket"}"#,
        )
        .json_ok();
        self.request(
            "POST",
            "/upload/storage/v1/b/other-bucket/o?uploadType=media&name=kept",
            "",
            b"kept",
        )
        .json_ok();
    }

    /// Starts a resumable upload to the test bucket and returns its session path.
    fn resumable_session(&self, name: &str) -> String {
        let session = self.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}"),
            "",
            b"{}",
        );
        session.expect(200);
        let location = session.headers["location"].parse::<Uri>().unwrap();
        location.path_and_query().unwrap().as_str().to_owned()
    }

    fn assert_other_intact(&self) {
        assert_eq!(
            self.request("GET", &format!("{OTHER_OBJECT}?alt=media"), "", b"")
                .body,
            b"kept"
        );
    }
}

#[test]
fn bucket_purge_is_opt_in_and_refuses_malformed_requests_without_changes() {
    let dir = TestDir::new();
    let disabled = Server::start(&dir, &[]);
    disabled.populate();
    disabled.purge(PURGE, "").json_reason(404, "notFound");
    drop(disabled);
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    for method in ["GET", "POST", "PUT", "PATCH"] {
        let refused = server.request(method, PURGE, "", b"");
        refused.json_reason(405, "methodNotAllowed");
        assert_eq!(refused.headers["allow"], "DELETE", "{method}");
    }
    for (path, body, status, reason) in [
        (
            "/_internal/buckets/test-bucket?force=true",
            b"".as_slice(),
            400,
            "invalidArgument",
        ),
        (PURGE, b"{}".as_slice(), 400, "invalidArgument"),
        (
            "/_internal/buckets/",
            b"".as_slice(),
            400,
            "invalidParameter",
        ),
        (
            "/_internal/buckets/Test-Bucket",
            b"".as_slice(),
            400,
            "invalidParameter",
        ),
        (
            "/_internal/buckets/test%2Fbucket",
            b"".as_slice(),
            400,
            "invalidParameter",
        ),
        (
            "/_internal/buckets/test-bucket/o",
            b"".as_slice(),
            400,
            "invalidParameter",
        ),
        (
            "/_internal/buckets/missing-bucket",
            b"".as_slice(),
            404,
            "notFound",
        ),
    ] {
        server
            .request("DELETE", path, "", body)
            .json_reason(status, reason);
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
                .body,
            b"original",
            "{path}"
        );
    }
    // The path segment is decoded as the JSON API decodes a bucket name.
    let purged = server
        .request("DELETE", "/_internal/buckets/test%2Dbucket", "", b"")
        .json_ok();
    assert_eq!(purged, json!({"objects": 1, "cleanupPending": false}));
    server.request("GET", OBJECT, "", b"").expect(404);
}

#[test]
fn bucket_purge_deletes_every_generation_and_session_past_holds_and_keeps_other_buckets() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, &["--admin-enabled", "true", "--backend", backend]);
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                "",
                br#"{"name":"test-bucket","versioning":{"enabled":true}}"#,
            )
            .json_ok();
        for body in [b"first".as_slice(), b"second"] {
            server
                .request(
                    "POST",
                    "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
                    "",
                    body,
                )
                .json_ok();
        }
        server
            .request("PATCH", OBJECT, "", br#"{"temporaryHold":true}"#)
            .json_ok();
        let session = server.resumable_session("pending");
        server
            .request(
                "PUT",
                &session,
                "Content-Range: bytes 0-262143/524288\r\n",
                &vec![b'a'; 256 * 1024],
            )
            .expect(308);
        server.populate_other();
        // The API keeps refusing what the purge removes.
        server
            .request("DELETE", "/storage/v1/b/test-bucket", "", b"")
            .expect(409);
        server.request("DELETE", OBJECT, "", b"").expect(403);
        let purged = server.purge(PURGE, "").json_ok();
        assert_eq!(
            purged,
            json!({"objects": 2, "cleanupPending": false}),
            "{backend}"
        );
        server
            .request("GET", "/storage/v1/b/test-bucket", "", b"")
            .expect(404);
        server
            .request("PUT", &session, "Content-Range: bytes */*\r\n", b"")
            .expect(404);
        if backend == "disk" {
            assert_eq!(common::process::staging_count(&dir), 0);
            assert_eq!(common::process::blob_count(&dir), 1);
        }
        server.assert_other_intact();
        // A bucket recreated under the name starts empty, without the old generations.
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                "",
                br#"{"name":"test-bucket"}"#,
            )
            .json_ok();
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o?versions=true", "", b"")
                .json_ok(),
            json!({"kind":"storage#objects"})
        );
        assert_eq!(
            server.purge(PURGE, "").json_ok(),
            json!({"objects": 0, "cleanupPending": false})
        );
        server.purge(PURGE, "").json_reason(404, "notFound");
        if backend == "disk" {
            drop(server);
            let server = Server::start(&dir, &["--admin-enabled", "true"]);
            server
                .request("GET", "/storage/v1/b/test-bucket", "", b"")
                .expect(404);
            server.assert_other_intact();
        }
    }
}

#[test]
fn bucket_purge_answers_503_while_a_session_of_the_bucket_writes_and_never_waits_for_others() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    server.populate_other();
    let session = server.resumable_session("pending");
    let mut chunk = TcpStream::connect(&server.address).unwrap();
    chunk
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(chunk, "PUT {session} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 262144\r\nContent-Range: bytes 0-262143/524288\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n").unwrap();
    let mut chunk = BufReader::new(chunk);
    // The server asks for the body once the request holds the session's writer.
    read_upload_go_ahead(&mut chunk);
    let busy = server.purge(PURGE, "");
    busy.json_reason(503, "backendError");
    assert_eq!(busy.headers["retry-after"], "1");
    assert_eq!(
        busy.json()["error"]["message"],
        "Purge made no changes because a request is writing to an upload session or appendable object of this bucket; retry after it completes"
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"original"
    );
    // An upload to another bucket in flight does not hold the purge back, and still finishes.
    let mut upload = pending_media_upload(&server, "late", b"late".len());
    assert_eq!(
        server
            .purge("/_internal/buckets/other-bucket", "")
            .json_ok(),
        json!({"objects": 1, "cleanupPending": false})
    );
    chunk.get_mut().write_all(&vec![b'a'; 256 * 1024]).unwrap();
    let written = common::read_http_response(&mut chunk, Body::Expected).unwrap();
    assert!(
        written.starts_with(b"HTTP/1.1 308"),
        "{}",
        String::from_utf8_lossy(&written)
    );
    upload.get_mut().write_all(b"late").unwrap();
    let finished = common::read_http_response(&mut upload, Body::Expected).unwrap();
    assert!(
        finished.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&finished)
    );
    assert_eq!(
        server.purge(PURGE, "").json_ok(),
        json!({"objects": 2, "cleanupPending": false})
    );
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn bucket_purge_uses_the_management_guards_and_the_local_manager() {
    let dir = TestDir::new();
    let registry = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"manager@example.invalid", "tokens":["manager"], "project_roles":{}},
            {"id":"cloud-admin@example.invalid", "tokens":["cloud-admin"], "project_roles":{"local":["roles/storage.admin"]}}
        ]}),
    );
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--admin-principal",
            "manager@example.invalid",
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "Authorization: Bearer cloud-admin\r\n",
            br#"{"name":"test-bucket"}"#,
        )
        .json_ok();
    // Browser and Host checks come first, then the manager; Google IAM roles confer nothing.
    for (headers, status) in [
        (
            "Origin: https://evil.example\r\nAuthorization: Bearer manager\r\n",
            403,
        ),
        (
            "Sec-Fetch-Site: cross-site\r\nAuthorization: Bearer manager\r\n",
            403,
        ),
        ("", 401),
        ("Authorization: Bearer unknown\r\n", 401),
        ("Authorization: Bearer cloud-admin\r\n", 403),
    ] {
        assert_eq!(server.purge(PURGE, headers).status, status, "{headers}");
    }
    assert_eq!(
        server.method_status_with_host(
            "DELETE",
            PURGE,
            "evil.example.com",
            "Authorization: Bearer manager\r\n",
            b""
        ),
        421
    );
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket",
            "Authorization: Bearer cloud-admin\r\n",
            b"",
        )
        .expect(200);
    assert_eq!(
        server
            .purge(PURGE, "Authorization: Bearer manager\r\n")
            .json_ok(),
        json!({"objects": 0, "cleanupPending": false})
    );
}

/// A virtual-hosted request names an XML object even when its path looks like the route.
#[test]
fn virtual_host_purge_lookalike_keeps_object_semantics() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--xml-virtual-host-domains",
            "storage.invalid",
        ],
    );
    server.populate();
    assert_eq!(
        server.method_status_with_host("DELETE", PURGE, "test-bucket.storage.invalid", "", b""),
        404
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"original"
    );
}

/// Directory synchronization maps a bucket it finds in the tree, so the purge refuses it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn bucket_purge_refuses_a_bucket_directory_synchronization_maps() {
    let (dir, sync) = (TestDir::new(), TestDir::new());
    std::fs::create_dir(sync.0.join("test-bucket")).unwrap();
    std::fs::write(sync.0.join("test-bucket/value"), b"preserve").unwrap();
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--sync-dir",
            sync.0.to_str().unwrap(),
        ],
    );
    common::input_fixtures::eventually_some("the tree's bucket to be ingested", || {
        (server.request("GET", OBJECT, "", b"").status == 200).then_some(())
    });
    let refused = server.purge(PURGE, "");
    refused.json_reason(409, "conflict");
    assert_eq!(
        std::fs::read(sync.0.join("test-bucket/value")).unwrap(),
        b"preserve"
    );
    server.request("GET", OBJECT, "", b"").expect(200);
}
