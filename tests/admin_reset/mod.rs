use super::*;

const CONFIRM: &[u8] = br#"{"confirm":"gcs-mimic"}"#;

#[test]
fn virtual_host_reset_lookalike_keeps_object_semantics_and_existing_instance_data() {
    for profile in ["native", "legacy-emulator"] {
        let dir = TestDir::new();
        let server = Server::start(
            &dir,
            &[
                "--admin-enabled",
                "true",
                "--compat-profile",
                profile,
                "--xml-virtual-host-domains",
                "storage.invalid",
            ],
        );
        server.populate();
        let host = "test-bucket.storage.invalid";
        for path in [
            "/_internal/reset",
            "/_internal/reseed",
            "/_internal/delete_all",
        ] {
            assert_eq!(
                server.method_status_with_host("PUT", path, host, "", CONFIRM),
                200
            );
            assert_eq!(server.status_with_host(path, host, ""), 200);
            server.method_status_with_host("POST", path, host, "", CONFIRM);
        }
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
                .body,
            b"original"
        );
    }
}

#[test]
fn reset_is_explicit_clears_session_history_and_preserves_revision_high_water_after_restart() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    server.populate();
    assert_eq!(
        server
            .request("POST", "/_internal/reset", "", CONFIRM)
            .status,
        404
    );
    drop(server);
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            "",
            br#"{"versioning":{"enabled":true}}"#,
        )
        .json();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            "",
            b"new version",
        )
        .json();
    let old = server.request("GET", OBJECT, "", b"").json();
    let policy = server
        .request("GET", "/storage/v1/b/test-bucket/iam", "", b"")
        .json();
    let session = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=pending",
        "",
        b"{}",
    );
    assert_eq!(session.status, 200);
    let location = session.headers["location"].parse::<Uri>().unwrap();
    let prefix = server.request(
        "PUT",
        location.path_and_query().unwrap().as_str(),
        "Content-Range: bytes 0-262143/524288\r\n",
        &vec![b'a'; 256 * 1024],
    );
    assert_eq!(
        prefix.status,
        308,
        "{}",
        String::from_utf8_lossy(&prefix.body)
    );
    for (method, path, body, status) in [
        ("GET", "/_internal/reset", b"".as_slice(), 405),
        ("POST", "/_internal/reset?confirm=gcs-mimic", CONFIRM, 400),
        ("POST", "/_internal/reset", b"{}".as_slice(), 400),
        (
            "POST",
            "/_internal/reset",
            br#"{"confirm":"wrong"}"#.as_slice(),
            400,
        ),
        (
            "POST",
            "/_internal/reset",
            br#"{"confirm":"gcs-mimic","unexpected":true}"#.as_slice(),
            400,
        ),
    ] {
        assert_eq!(server.request(method, path, "", body).status, status);
        assert_eq!(server.request("GET", OBJECT, "", b"").json(), old);
    }
    let reset = server
        .request("POST", "/_internal/reset", "", CONFIRM)
        .json();
    assert_eq!(
        reset,
        json!({"buckets":1, "objects":2, "cleanupPending":false})
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    assert_eq!(server.request("GET", OBJECT, "", b"").status, 404);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=local", "", b"")
            .json(),
        json!({"kind":"storage#buckets"})
    );
    assert_eq!(
        server
            .request(
                "PUT",
                location.path_and_query().unwrap().as_str(),
                "Content-Range: bytes */*\r\n",
                b""
            )
            .status,
        404
    );
    server.populate();
    let fresh = server.request("GET", OBJECT, "", b"").json();
    assert!(
        fresh["generation"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .unwrap()
            > old["generation"].as_str().unwrap().parse::<i64>().unwrap()
    );
    assert_ne!(
        server
            .request("GET", "/storage/v1/b/test-bucket/iam", "", b"")
            .json()["etag"],
        policy["etag"]
    );
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "{OBJECT}?generation={}",
                    old["generation"].as_str().unwrap()
                ),
                "",
                b""
            )
            .status,
        404
    );
    server
        .request("POST", "/_internal/reset", "", CONFIRM)
        .json();
    drop(server);
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    assert_eq!(server.request("GET", OBJECT, "", b"").status, 404);
}

#[test]
fn reset_rejects_active_upload_without_changing_existing_bytes() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=pending HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n").unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(line.contains("100 Continue"), "{line}");
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "\r\n");
    let busy = server.request("POST", "/_internal/reset", "", CONFIRM);
    assert_eq!(busy.status, 503);
    assert_eq!(busy.headers["retry-after"], "1");
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"original"
    );
    reader.get_mut().write_all(b"hello").unwrap();
    let reply = common::read_http_response(&mut reader, false).unwrap();
    assert!(reply.starts_with(b"HTTP/1.1 200"));
    drop(reader);
    server
        .request("POST", "/_internal/reset", "", CONFIRM)
        .json();
    assert_eq!(server.request("GET", OBJECT, "", b"").status, 404);
}

#[test]
fn reset_uses_the_local_manager_and_refuses_active_directory_sync() {
    let dir = TestDir::new();
    let registry = dir.0.join("identities.json");
    std::fs::write(&registry, json!({"identities":[
        {"id":"manager@example.invalid", "tokens":["manager"], "project_roles":{}},
        {"id":"cloud-admin@example.invalid", "tokens":["cloud-admin"], "project_roles":{"local":["roles/storage.admin"]}}
    ]}).to_string()).unwrap();
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
        .json();
    for (headers, status) in [
        ("", 401),
        ("Authorization: Bearer unknown\r\n", 401),
        ("Authorization: Bearer cloud-admin\r\n", 403),
    ] {
        assert_eq!(
            server
                .request("POST", "/_internal/reset", headers, CONFIRM)
                .status,
            status
        );
    }
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket",
                "Authorization: Bearer cloud-admin\r\n",
                b""
            )
            .status,
        200
    );
    server
        .request(
            "POST",
            "/_internal/reset",
            "Authorization: Bearer manager\r\n",
            CONFIRM,
        )
        .json();
    drop(server);
    let sync = TestDir::new();
    std::fs::write(sync.0.join("external-source.txt"), b"preserve").unwrap();
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--sync-dir",
            sync.0.to_str().unwrap(),
        ],
    );
    server.populate();
    assert_eq!(
        server
            .request("POST", "/_internal/reset", "", CONFIRM)
            .status,
        409
    );
    assert_eq!(
        std::fs::read(sync.0.join("external-source.txt")).unwrap(),
        b"preserve"
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"original"
    );
}
