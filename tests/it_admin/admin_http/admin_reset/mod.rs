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
fn reset_rejects_active_upload_without_changing_existing_bytes() {
    assert_active_upload_reset("30000");
}

#[test]
fn reset_short_catalog_wait_reports_no_changes() {
    assert_active_upload_reset("500");
}

fn assert_active_upload_reset(catalog_wait_ms: &str) {
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
            catalog_wait_ms,
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
    assert!(reply.starts_with(b"HTTP/1.1 200"));
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
