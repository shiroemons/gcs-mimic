use super::*;

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
        .json();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            "",
            b"API edit",
        )
        .json();
    let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    let count = || {
        connection
            .query_row("SELECT count(*) FROM notification_outbox", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(count(), 2);
    server.request("POST", "/_internal/reseed", "", b"").json();
    assert_eq!(count(), 2);
    server
        .fixture(
            json!({"bucket":"test-bucket", "name":"value"}),
            b"runtime",
            "",
        )
        .json();
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
            .json();
        server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=unrelated",
                "",
                b"keep",
            )
            .json();
        let listing = server
            .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
            .json();
        std::fs::create_dir(source.0.join("new-empty-bucket")).unwrap();
        std::fs::create_dir(source.0.join("z-Invalid")).unwrap();
        std::fs::write(source.0.join("z-Invalid/bad"), b"invalid").unwrap();
        assert_eq!(
            server.request("POST", "/_internal/reseed", "", b"").status,
            503
        );
        assert_eq!(server.request("GET", OBJECT, "", b"").json(), old);
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
                .json(),
            listing
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/new-empty-bucket", "", b"")
                .status,
            404
        );
        std::fs::remove_dir_all(source.0.join("z-Invalid")).unwrap();
        let replay = server.request("PUT", "/_internal/reseed", "", b"").json();
        assert_eq!(replay["outcome"], "imported");
        let updated = server.request("GET", OBJECT, "", b"").json();
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
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/new-empty-bucket", "", b"")
                .status,
            200
        );
        assert_eq!(
            server
                .request("POST", "/_internal/delete_all", "", b"")
                .status,
            200
        );
        assert_eq!(server.request("GET", OBJECT, "", b"").status, 404);
        server.request("POST", "/_internal/reseed", "", b"").json();
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
        assert_eq!(server.request("POST", path, "", b"").status, 404);
    }
    drop(server);
    let registry = data.0.join("identities.json");
    std::fs::write(&registry, json!({"identities":[
        {"id":"manager@example.invalid","tokens":["manager"],"project_roles":{}},
        {"id":"cloud-admin@example.invalid","tokens":["cloud-admin"],"project_roles":{"local":["roles/storage.admin"]}}
    ]}).to_string()).unwrap();
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
        assert_eq!(server.request("GET", path, manager, b"").status, 405);
        assert_eq!(
            server
                .request("POST", path, manager, br#"{"path":"untrusted"}"#)
                .status,
            400
        );
        assert_eq!(
            server
                .request("POST", &format!("{path}?path=untrusted"), manager, b"")
                .status,
            400
        );
        assert_eq!(
            server
                .request("POST", path, manager, &vec![b' '; 65537])
                .status,
            413
        );
    }
    assert_eq!(
        server
            .request("PUT", "/_internal/delete_all", manager, b"")
            .status,
        405
    );
    assert_eq!(
        server
            .request("POST", "/_internal/reseed", manager, b"")
            .status,
        200
    );
    assert_eq!(server.request("GET", OBJECT, manager, b"").status, 403);
    assert_eq!(
        server
            .request("POST", "/_internal/delete_all", manager, b"")
            .status,
        200
    );
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
        .json();
    let listing = server
        .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
        .json();
    std::fs::write(source.0.join("test-bucket/value"), b"changed").unwrap();
    std::fs::write(source.0.join("test-bucket/a-first"), b"changed").unwrap();
    assert_eq!(
        server.request("POST", "/_internal/reseed", "", b"").status,
        403
    );
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), original);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
            .json(),
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
