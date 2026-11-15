use super::*;

const CONFIRM: &[u8] = br#"{"confirm":"gcs-mimic"}"#;
const TO_STARTUP: &[u8] = br#"{"confirm":"gcs-mimic","state":"startup"}"#;

/// A seed tree with one bucket that the buckets document configures and one it does not.
fn startup_fixtures(source: &TestDir, document: &std::path::Path) {
    std::fs::create_dir_all(source.0.join("configured-bucket/nested")).unwrap();
    std::fs::write(
        source.0.join("configured-bucket/nested/report.csv"),
        b"seed",
    )
    .unwrap();
    std::fs::create_dir(source.0.join("seed-only-bucket")).unwrap();
    std::fs::write(source.0.join("seed-only-bucket/value"), b"other").unwrap();
    std::fs::write(
        document,
        json!({"version":1,"buckets":[{"name":"configured-bucket",
            "labels":{"team":"tests"},"versioning":{"enabled":true}}]})
        .to_string(),
    )
    .unwrap();
}

/// A reset to the startup state rebuilds what startup created, on both backends: the buckets
/// document's buckets with their settings and the seed tree in them, nothing else. Protection
/// does not refuse it, and the document startup read wins over the file's current content.
#[test]
fn reset_to_the_startup_state_restores_configured_buckets_and_seed_on_both_backends() {
    for backend in ["disk", "memory"] {
        let (data, source, config) = (TestDir::new(), TestDir::new(), TestDir::new());
        let document = config.0.join("buckets.json");
        startup_fixtures(&source, &document);
        let server = Server::start(
            &data,
            &[
                "--admin-enabled",
                "true",
                "--backend",
                backend,
                "--seed-dir",
                source.0.to_str().unwrap(),
                "--buckets-file",
                document.to_str().unwrap(),
            ],
        );
        let seeded = "/storage/v1/b/configured-bucket/o/nested%2Freport.csv";
        let started = server.request("GET", seeded, "", b"").json_ok();
        server
            .request(
                "PATCH",
                "/storage/v1/b/configured-bucket",
                "",
                br#"{"labels":{"team":"changed"}}"#,
            )
            .json_ok();
        server
            .request(
                "POST",
                "/upload/storage/v1/b/configured-bucket/o?uploadType=media&name=extra",
                "",
                b"held",
            )
            .json_ok();
        server
            .request(
                "PATCH",
                "/storage/v1/b/configured-bucket/o/extra",
                "",
                br#"{"temporaryHold":true}"#,
            )
            .json_ok();
        server
            .request("DELETE", "/storage/v1/b/seed-only-bucket/o/value", "", b"")
            .expect(204);
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                "",
                br#"{"name":"api-bucket"}"#,
            )
            .json_ok();
        // A file edited after startup is not what startup applied.
        std::fs::write(
            &document,
            json!({"version":1,"buckets":[{"name":"late-bucket"}]}).to_string(),
        )
        .unwrap();
        let answer = server
            .request("POST", "/_internal/reset", "", TO_STARTUP)
            .json_ok();
        // Removed: the three buckets, the seeded file the API kept and the held upload.
        assert_eq!(
            answer,
            json!({"buckets":3,"objects":2,"restored":{"buckets":2,"objects":2}}),
            "{backend}"
        );
        let bucket = server
            .request("GET", "/storage/v1/b/configured-bucket", "", b"")
            .json_ok();
        assert_eq!(bucket["labels"], json!({"team":"tests"}), "{backend}");
        assert_eq!(bucket["versioning"]["enabled"], true, "{backend}");
        let restored = server.request("GET", seeded, "", b"").json_ok();
        assert_eq!(restored["contentType"], started["contentType"]);
        assert_eq!(restored["md5Hash"], started["md5Hash"]);
        assert_eq!(
            server
                .request(
                    "GET",
                    "/storage/v1/b/seed-only-bucket/o/value?alt=media",
                    "",
                    b""
                )
                .body,
            b"other"
        );
        for gone in [
            "/storage/v1/b/configured-bucket/o/extra",
            "/storage/v1/b/api-bucket",
            "/storage/v1/b/late-bucket",
        ] {
            server
                .request("GET", gone, "", b"")
                .expect_with(404, format!("{backend} {gone}"));
        }
    }
}

/// A seed tree that can no longer be published refuses the whole reset before anything is
/// cleared, and the answer names the reset rather than a reseed.
#[test]
fn a_reset_to_an_unpublishable_startup_state_changes_nothing() {
    let (data, source, config) = (TestDir::new(), TestDir::new(), TestDir::new());
    let document = config.0.join("buckets.json");
    startup_fixtures(&source, &document);
    let server = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
            "--buckets-file",
            document.to_str().unwrap(),
        ],
    );
    server
        .request(
            "POST",
            "/upload/storage/v1/b/configured-bucket/o?uploadType=media&name=kept",
            "",
            b"kept",
        )
        .json_ok();
    std::fs::create_dir(source.0.join("Invalid-Bucket")).unwrap();
    std::fs::write(source.0.join("Invalid-Bucket/value"), b"invalid").unwrap();
    let rejected = server.request("POST", "/_internal/reset", "", TO_STARTUP);
    let message = rejected.json_reason_value(400, "invalid")["error"]["message"].clone();
    assert!(
        message
            .as_str()
            .is_some_and(|text| text.starts_with("Reset made no changes")),
        "{message}"
    );
    assert!(!rejected.headers.contains_key("retry-after"));
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/configured-bucket/o/kept?alt=media",
                "",
                b""
            )
            .body,
        b"kept"
    );
}

/// Without a buckets document or seed tree the startup state is empty, an omitted or `empty`
/// state keeps the plain reset, and an unknown state changes nothing.
#[test]
fn reset_states_are_explicit_and_an_unconfigured_startup_state_is_empty() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    server
        .request(
            "POST",
            "/_internal/reset",
            "",
            br#"{"confirm":"gcs-mimic","state":"pristine"}"#,
        )
        .json_reason(400, "invalidArgument");
    server.request("GET", OBJECT, "", b"").json_ok();
    let empty = server
        .request(
            "POST",
            "/_internal/reset",
            "",
            br#"{"confirm":"gcs-mimic","state":"empty"}"#,
        )
        .json_ok();
    assert_eq!(empty["buckets"], 1);
    assert_eq!(empty["cleanupPending"], false);
    server.populate();
    assert_eq!(
        server
            .request("POST", "/_internal/reset", "", TO_STARTUP)
            .json_ok(),
        json!({"buckets":1,"objects":1,"restored":{"buckets":0,"objects":0}})
    );
    server
        .request("GET", "/storage/v1/b/test-bucket", "", b"")
        .expect(404);
}

#[test]
fn virtual_host_reset_lookalike_keeps_object_semantics_and_existing_instance_data() {
    for profile in ["native", "fake-gcs-server"] {
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
    server
        .request("POST", "/_internal/reset", "", CONFIRM)
        .expect(404);
    drop(server);
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            "",
            br#"{"versioning":{"enabled":true}}"#,
        )
        .json_ok();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            "",
            b"new version",
        )
        .json_ok();
    let old = server.request("GET", OBJECT, "", b"").json_ok();
    let policy = server
        .request("GET", "/storage/v1/b/test-bucket/iam", "", b"")
        .json_ok();
    let session = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=pending",
        "",
        b"{}",
    );
    session.expect(200);
    let location = session.headers["location"].parse::<Uri>().unwrap();
    let prefix = server.request(
        "PUT",
        location.path_and_query().unwrap().as_str(),
        "Content-Range: bytes 0-262143/524288\r\n",
        &vec![b'a'; 256 * 1024],
    );
    prefix.expect(308);
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
        assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), old);
    }
    let reset = server
        .request("POST", "/_internal/reset", "", CONFIRM)
        .json_ok();
    assert_eq!(
        reset,
        json!({"buckets":1, "objects":2, "cleanupPending":false})
    );
    assert_eq!(common::process::staging_count(&dir), 0);
    server.request("GET", OBJECT, "", b"").expect(404);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=local", "", b"")
            .json_ok(),
        json!({"kind":"storage#buckets"})
    );
    server
        .request(
            "PUT",
            location.path_and_query().unwrap().as_str(),
            "Content-Range: bytes */*\r\n",
            b"",
        )
        .expect(404);
    server.populate();
    let fresh = server.request("GET", OBJECT, "", b"").json_ok();
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
            .json_ok()["etag"],
        policy["etag"]
    );
    server
        .request(
            "GET",
            &format!(
                "{OBJECT}?generation={}",
                old["generation"].as_str().unwrap()
            ),
            "",
            b"",
        )
        .expect(404);
    server
        .request("POST", "/_internal/reset", "", CONFIRM)
        .json_ok();
    drop(server);
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.request("GET", OBJECT, "", b"").expect(404);
}

#[test]
fn requests_during_a_reset_wait_and_then_see_the_cleared_state() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let mut active = pending_media_upload(&server, "pending", b"hello".len());
    let (reply, mut held_back) = std::thread::scope(|scope| {
        let reset = scope.spawn(|| server.request("POST", "/_internal/reset", "", CONFIRM));
        let held_back = held_back_media_upload(&server, "held-back", b"fresh".len());
        // The reset is still waiting, a second and more after it started.
        assert!(!reset.is_finished());
        // Only new uploads line up behind it; reads are not stopped by a stalled upload.
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
                .body,
            b"original"
        );
        active.get_mut().write_all(b"hello").unwrap();
        let finished = common::read_http_response(&mut active, Body::Expected).unwrap();
        assert!(finished.starts_with(b"HTTP/1.1 200"));
        (reset.join().unwrap(), held_back)
    });
    reply.json_ok();
    // The bucket went with the reset, so the upload is refused for that, before its body.
    let reply = common::read_http_response(&mut held_back, Body::Expected).unwrap();
    assert!(
        reply.starts_with(b"HTTP/1.1 404"),
        "{}",
        String::from_utf8_lossy(&reply)
    );
    server.request("GET", OBJECT, "", b"").expect(404);
}

/// Twice the 32 callers the management line once held before refusing the next one.
const MANY_RESETS: usize = 64;

#[test]
fn management_calls_past_every_slot_wait_their_turn_and_all_succeed() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let mut active = pending_media_upload(&server, "pending", b"hello".len());
    let replies = std::thread::scope(|scope| {
        let resets: Vec<_> = (0..MANY_RESETS)
            .map(|_| scope.spawn(|| server.request("POST", "/_internal/reset", "", CONFIRM)))
            .collect();
        drop(held_back_media_upload(&server, "held-back", b"fresh".len()));
        // A second and more on, an immediate refusal has already answered.
        assert!(resets.iter().all(|reset| !reset.is_finished()));
        active.get_mut().write_all(b"hello").unwrap();
        let finished = common::read_http_response(&mut active, Body::Expected).unwrap();
        assert!(finished.starts_with(b"HTTP/1.1 200"));
        resets
            .into_iter()
            .map(|reset| reset.join().unwrap())
            .collect::<Vec<_>>()
    });
    for reply in replies {
        reply.json_ok();
    }
    server.request("GET", OBJECT, "", b"").expect(404);
}

#[test]
fn a_disconnected_reset_gives_up_its_wait_and_changes_nothing() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let _active = pending_media_upload(&server, "pending", b"hello".len());
    let mut reset = TcpStream::connect(&server.address).unwrap();
    write!(
        reset,
        "POST /_internal/reset HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        CONFIRM.len()
    )
    .unwrap();
    reset.write_all(CONFIRM).unwrap();
    drop(held_back_media_upload(&server, "held-back", b"fresh".len()));
    drop(reset);
    // The first upload is still active, so only the end of the wait can let this one through.
    common::input_fixtures::eventually_some("uploads to be let through again", || {
        let upload = start_media_upload(&server, "let-through", b"fresh".len());
        upload_is_held_back(upload).is_none().then_some(())
    });
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"original"
    );
}

#[test]
fn reset_short_catalog_wait_reports_no_changes() {
    let dir = TestDir::new();
    // The catalog wait applies to every request, so on a loaded runner a short one also
    // times out the ordinary setup uploads, which are not under test. Populate with the
    // default wait, then restart on the same data with the wait under test.
    let setup = Server::start(&dir, &["--admin-enabled", "true"]);
    setup.populate();
    // A graceful server stops on drop; an explicit stop would signal it twice.
    drop(setup);
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--catalog-wait-timeout-ms",
            "2000",
        ],
    );
    let mut reader = pending_media_upload(&server, "pending", b"hello".len());
    let busy = server.request("POST", "/_internal/reset", "", CONFIRM);
    busy.expect(503);
    assert_eq!(busy.headers["retry-after"], "1");
    assert_eq!(
        busy.json()["error"]["message"],
        "Reset made no changes because catalog work or an unpublished body is active; retry after it completes"
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"original"
    );
    reader.get_mut().write_all(b"hello").unwrap();
    let reply = common::read_http_response(&mut reader, Body::Expected).unwrap();
    assert!(
        reply.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&reply)
    );
    drop(reader);
    // The finished upload's cleanup can still hold the catalog past a short wait; reset
    // then answers its documented no-change 503 with Retry-After, so retry it.
    crate::common::input_fixtures::eventually_some("reset after the upload finished", || {
        let reply = server.request("POST", "/_internal/reset", "", CONFIRM);
        (reply.status != 503).then_some(reply)
    })
    .json_ok();
    server.request("GET", OBJECT, "", b"").expect(404);
}

#[test]
fn reset_uses_the_local_manager_and_refuses_active_directory_sync() {
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
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket",
            "Authorization: Bearer cloud-admin\r\n",
            b"",
        )
        .expect(200);
    server
        .request(
            "POST",
            "/_internal/reset",
            "Authorization: Bearer manager\r\n",
            CONFIRM,
        )
        .json_ok();
    drop(server);
    // Directory synchronization runs only on Linux and macOS; elsewhere startup rejects it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
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
        server
            .request("POST", "/_internal/reset", "", CONFIRM)
            .expect(409);
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
}
