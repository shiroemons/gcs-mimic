use super::*;

fn seeded_server(data: &TestDir, source: &TestDir, catalog_wait_ms: &str) -> Server {
    std::fs::create_dir(source.0.join("test-bucket")).unwrap();
    std::fs::write(source.0.join("test-bucket/value"), b"seed").unwrap();
    let server = Server::start(
        data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
            "--catalog-wait-timeout-ms",
            catalog_wait_ms,
        ],
    );
    // The catalog wait applies to every request, so on a loaded runner a short one also
    // times out this setup upload, which is not under test. Its 503 is retryable; a
    // retry may add a generation, and only the content is checked later. Restarting with
    // the default wait is no option: startup replays the seed over the edit.
    common::input_fixtures::eventually_some("the setup edit", || {
        let reply = server.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            "",
            b"API edit",
        );
        (reply.status != 503).then_some(reply)
    })
    .json_ok();
    server
}

fn finish_upload(mut upload: BufReader<TcpStream>) {
    upload.get_mut().write_all(b"hello").unwrap();
    let reply = common::read_http_response(&mut upload, Body::Expected).unwrap();
    assert!(reply.starts_with(b"HTTP/1.1 200"));
}

#[test]
fn reseed_waits_for_an_active_upload_and_then_replays_fixtures() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let server = seeded_server(&data, &source, "30000");
    let upload = pending_media_upload(&server, "pending", b"hello".len());
    let replay = std::thread::scope(|scope| {
        let reseed = scope.spawn(|| server.request("POST", "/_internal/reseed", "", b""));
        // Reseed must still be waiting when the upload completes; an immediate refusal fails here.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!reseed.is_finished());
        finish_upload(upload);
        reseed.join().unwrap()
    });
    assert_eq!(replay.json_ok()["outcome"], "imported");
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"seed"
    );
}

#[test]
fn reseed_short_catalog_wait_reports_no_changes_while_an_upload_is_active() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let server = seeded_server(&data, &source, "500");
    let upload = pending_media_upload(&server, "pending", b"hello".len());
    let busy = server.request("POST", "/_internal/reseed", "", b"");
    busy.expect(503);
    assert_eq!(busy.headers["retry-after"], "1");
    assert_eq!(
        busy.json()["error"]["message"],
        "Reseed made no changes because catalog work, an unpublished body or another publication is active; retry after it completes"
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"API edit"
    );
    finish_upload(upload);
    // The finished upload's cleanup can still hold the catalog past a short wait; reseed
    // then answers its documented no-change 503 with Retry-After, so retry it.
    crate::common::input_fixtures::eventually_some("reseed after the upload finished", || {
        let reply = server.request("POST", "/_internal/reseed", "", b"");
        (reply.status != 503).then_some(reply)
    })
    .json_ok();
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"seed"
    );
}

#[test]
fn disk_reseed_suppresses_only_fixture_events_and_keeps_runtime_delivery_selection() {
    let data = TestDir::new();
    let source = TestDir::new();
    std::fs::create_dir(source.0.join("test-bucket")).unwrap();
    std::fs::write(source.0.join("test-bucket/value"), b"seed").unwrap();
    let server = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
        ],
    );
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/notificationConfigs",
            "",
            br#"{"topic":"projects/local/topics/events"}"#,
        )
        .json_ok();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            "",
            b"API edit",
        )
        .json_ok();
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    let count = || common::catalog::count(&connection, "notification_outbox");
    assert_eq!(count(), 2);
    server
        .request("POST", "/_internal/reseed", "", b"")
        .json_ok();
    assert_eq!(count(), 2);
    server
        .fixture(
            json!({"bucket":"test-bucket", "name":"value"}),
            b"runtime",
            "",
        )
        .json_ok();
    assert_eq!(count(), 4);
}

#[test]
fn configured_reseed_is_atomic_and_uses_disk_merge_or_ram_replacement() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let source = TestDir::new();
        let bucket = source.0.join("test-bucket");
        std::fs::create_dir(&bucket).unwrap();
        std::fs::write(bucket.join("value"), b"seed").unwrap();
        std::fs::write(bucket.join("value.metadata"), b"literal-object").unwrap();
        let server = Server::start(
            &data,
            &[
                "--admin-enabled",
                "true",
                "--backend",
                backend,
                "--seed-dir",
                source.0.to_str().unwrap(),
                "--api-base-url",
                "http://fixture.invalid",
            ],
        );
        let old = server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
                "",
                b"API edit",
            )
            .json_ok();
        server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=unrelated",
                "",
                b"keep",
            )
            .json_ok();
        let listing = server
            .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
            .json_ok();
        std::fs::create_dir(source.0.join("new-empty-bucket")).unwrap();
        std::fs::create_dir(source.0.join("z-Invalid")).unwrap();
        std::fs::write(source.0.join("z-Invalid/bad"), b"invalid").unwrap();
        server
            .request("POST", "/_internal/reseed", "", b"")
            .expect(503);
        assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), old);
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
                .json_ok(),
            listing
        );
        server
            .request("GET", "/storage/v1/b/new-empty-bucket", "", b"")
            .expect(404);
        std::fs::remove_dir_all(source.0.join("z-Invalid")).unwrap();
        let replay = server
            .request("PUT", "/_internal/reseed", "", b"")
            .json_ok();
        assert_eq!(replay["outcome"], "imported");
        let updated = server.request("GET", OBJECT, "", b"").json_ok();
        assert!(
            updated["generation"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()
                > old["generation"].as_str().unwrap().parse::<i64>().unwrap()
        );
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
                .body,
            b"seed"
        );
        assert_eq!(
            server
                .request(
                    "GET",
                    "/storage/v1/b/test-bucket/o/value.metadata?alt=media",
                    "",
                    b""
                )
                .body,
            b"literal-object"
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o/unrelated", "", b"")
                .status,
            if backend == "disk" { 200 } else { 404 }
        );
        server
            .request("GET", "/storage/v1/b/new-empty-bucket", "", b"")
            .expect(200);
        server
            .request("POST", "/_internal/delete_all", "", b"")
            .expect(200);
        server.request("GET", OBJECT, "", b"").expect(404);
        server
            .request("POST", "/_internal/reseed", "", b"")
            .json_ok();
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
                .body,
            b"seed"
        );
    }
}

#[test]
fn source_management_methods_bodies_and_authority_are_explicit() {
    let data = TestDir::new();
    let source = TestDir::new();
    std::fs::create_dir(source.0.join("test-bucket")).unwrap();
    std::fs::write(source.0.join("test-bucket/value"), b"seed").unwrap();
    let seed = source.0.to_str().unwrap();
    let server = Server::start(&data, &["--seed-dir", seed]);
    for path in ["/_internal/reseed", "/_internal/delete_all"] {
        server.request("POST", path, "", b"").expect(404);
    }
    drop(server);
    let registry = common::process::write_identities(
        &data.0,
        &json!({"identities":[
            {"id":"manager@example.invalid","tokens":["manager"],"project_roles":{}},
            {"id":"cloud-admin@example.invalid","tokens":["cloud-admin"],"project_roles":{"local":["roles/storage.admin"]}}
        ]}),
    );
    let server = Server::start(
        &data,
        &[
            "--seed-dir",
            seed,
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
    let manager = "Authorization: Bearer manager\r\n";
    for path in ["/_internal/reseed", "/_internal/delete_all"] {
        for (auth, status) in [
            ("", 401),
            ("Authorization: Bearer unknown\r\n", 401),
            ("Authorization: Bearer cloud-admin\r\n", 403),
        ] {
            assert_eq!(server.request("POST", path, auth, b"").status, status);
        }
        server.request("GET", path, manager, b"").expect(405);
        server
            .request("POST", path, manager, br#"{"path":"untrusted"}"#)
            .expect(400);
        server
            .request("POST", &format!("{path}?path=untrusted"), manager, b"")
            .expect(400);
        server
            .request("POST", path, manager, &vec![b' '; 65537])
            .expect(413);
    }
    server
        .request("PUT", "/_internal/delete_all", manager, b"")
        .expect(405);
    server
        .request("POST", "/_internal/reseed", manager, b"")
        .expect(200);
    server.request("GET", OBJECT, manager, b"").expect(403);
    server
        .request("POST", "/_internal/delete_all", manager, b"")
        .expect(200);
}

#[test]
fn reseed_rejects_protection_without_changing_any_selected_objects() {
    let data = TestDir::new();
    let source = TestDir::new();
    std::fs::create_dir(source.0.join("test-bucket")).unwrap();
    std::fs::write(source.0.join("test-bucket/value"), b"seed").unwrap();
    std::fs::write(source.0.join("test-bucket/a-first"), b"first").unwrap();
    let server = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
        ],
    );
    let original = server
        .request("PATCH", OBJECT, "", br#"{"temporaryHold":true}"#)
        .json_ok();
    let listing = server
        .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
        .json_ok();
    std::fs::write(source.0.join("test-bucket/value"), b"changed").unwrap();
    std::fs::write(source.0.join("test-bucket/a-first"), b"changed").unwrap();
    server
        .request("POST", "/_internal/reseed", "", b"")
        .expect(403);
    assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), original);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
            .json_ok(),
        listing
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/a-first?alt=media",
                "",
                b""
            )
            .body,
        b"first"
    );
}
