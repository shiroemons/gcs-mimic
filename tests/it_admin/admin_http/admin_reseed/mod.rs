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
    finish_upload_with(&mut upload, b"hello");
}

fn finish_upload_with(upload: &mut BufReader<TcpStream>, body: &[u8]) {
    upload.get_mut().write_all(body).unwrap();
    let reply = common::read_http_response(upload, Body::Expected).unwrap();
    assert!(
        reply.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&reply)
    );
}

/// The catalog wait bounds waiting for other work, not how long a tree takes to import.
#[test]
fn a_reseed_may_take_longer_than_the_catalog_wait() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let server = seeded_server(&data, &source, "50");
    for index in 0..40 {
        std::fs::write(source.0.join(format!("test-bucket/extra-{index}")), b"seed").unwrap();
    }
    // A wait this short can still expire on a loaded runner, which is a retryable 503.
    let reply = common::input_fixtures::eventually_some("the reseed", || {
        let reply = server.request("POST", "/_internal/reseed", "", b"");
        (reply.status != 503).then_some(reply)
    });
    assert_eq!(reply.json_ok()["outcome"], "imported");
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/extra-39?alt=media",
                "",
                b""
            )
            .body,
        b"seed"
    );
}

#[test]
fn reseed_waits_for_an_active_upload_and_then_replays_fixtures() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let server = seeded_server(&data, &source, "30000");
    let upload = pending_media_upload(&server, "pending", b"hello".len());
    let (replay, mut held_back) = std::thread::scope(|scope| {
        let reseed = scope.spawn(|| server.request("POST", "/_internal/reseed", "", b""));
        let held_back = held_back_media_upload(&server, "held-back", b"fresh".len());
        // Reseed must still be waiting when the upload completes; an immediate refusal fails here.
        assert!(!reseed.is_finished());
        // Only new uploads line up behind it; reads are not stopped by a stalled upload.
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
                .body,
            b"API edit"
        );
        finish_upload(upload);
        (reseed.join().unwrap(), held_back)
    });
    assert_eq!(replay.json_ok()["outcome"], "imported");
    read_upload_go_ahead(&mut held_back);
    finish_upload_with(&mut held_back, b"fresh");
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
                "/storage/v1/b/test-bucket/o/held-back?alt=media",
                "",
                b""
            )
            .body,
        b"fresh"
    );
}

/// Past the two reseeds the server once held before refusing the next one.
const MANY_RESEEDS: usize = 8;

#[test]
fn reseeds_past_the_active_one_take_turns_and_all_complete() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let server = seeded_server(&data, &source, "60000");
    let upload = pending_media_upload(&server, "pending", b"hello".len());
    let replies = std::thread::scope(|scope| {
        let reseeds: Vec<_> = (0..MANY_RESEEDS)
            .map(|_| scope.spawn(|| server.request("POST", "/_internal/reseed", "", b"")))
            .collect();
        let mut held_back = held_back_media_upload(&server, "held-back", b"fresh".len());
        // A second and more on, an immediate refusal has already answered.
        assert!(reseeds.iter().all(|reseed| !reseed.is_finished()));
        finish_upload(upload);
        read_upload_go_ahead(&mut held_back);
        finish_upload_with(&mut held_back, b"fresh");
        reseeds
            .into_iter()
            .map(|reseed| reseed.join().unwrap())
            .collect::<Vec<_>>()
    });
    for reply in replies {
        assert_eq!(reply.json_ok()["outcome"], "imported");
    }
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
    let server = seeded_server(&data, &source, "2000");
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
        // Retrying cannot fix the tree, so the reply is a client error without Retry-After.
        let rejected = server.request("POST", "/_internal/reseed", "", b"");
        rejected.json_reason(400, "invalid");
        assert!(!rejected.headers.contains_key("retry-after"));
        let message = rejected.json()["error"]["message"].to_string();
        assert!(message.contains("not a valid bucket name"), "{message}");
        assert!(!message.contains("z-Invalid"), "{message}");
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

/// A tree that grew past a configured limit after startup fails the same way on every attempt:
/// reseed answers 400 naming the setting, without Retry-After or any path, and changes nothing.
#[test]
fn reseed_past_a_configured_limit_answers_400_naming_the_setting() {
    /// A limit the startup tree fits, and how the tree then grows past it.
    struct PastLimit {
        setting: &'static str,
        limit: &'static str,
        grow: fn(&std::path::Path),
    }
    let cases = [
        PastLimit {
            setting: "max_seed_entries",
            limit: "3",
            grow: |root| {
                for name in ["first-extra", "second-extra"] {
                    std::fs::write(root.join("test-bucket").join(name), b"seed").unwrap();
                }
            },
        },
        PastLimit {
            setting: "max_seed_depth",
            limit: "1",
            grow: |root| {
                std::fs::create_dir_all(root.join("test-bucket/level-one/level-two")).unwrap();
                std::fs::write(root.join("test-bucket/level-one/level-two/deep"), b"seed").unwrap();
            },
        },
        PastLimit {
            setting: "max_buckets_per_batch",
            limit: "1",
            grow: |root| std::fs::create_dir(root.join("second-bucket")).unwrap(),
        },
    ];
    for PastLimit {
        setting,
        limit,
        grow,
    } in cases
    {
        let (data, source) = (TestDir::new(), TestDir::new());
        std::fs::create_dir(source.0.join("test-bucket")).unwrap();
        std::fs::write(source.0.join("test-bucket/value"), b"seed").unwrap();
        let flag = format!("--{}", setting.replace('_', "-"));
        let server = Server::start(
            &data,
            &[
                "--admin-enabled",
                "true",
                "--seed-dir",
                source.0.to_str().unwrap(),
                &flag,
                limit,
            ],
        );
        let before = server.request("GET", OBJECT, "", b"").json_ok();
        grow(&source.0);
        let rejected = server.request("POST", "/_internal/reseed", "", b"");
        rejected.json_reason(400, "invalid");
        assert!(
            !rejected.headers.contains_key("retry-after"),
            "{setting}: {:?}",
            rejected.headers
        );
        let message = rejected.json()["error"]["message"].to_string();
        assert!(message.contains(setting), "{setting}: {message}");
        for leaked in [
            source.0.to_str().unwrap(),
            "extra",
            "level-",
            "second-bucket",
        ] {
            assert!(!message.contains(leaked), "{setting}: {message}");
        }
        assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), before);
        server
            .request("GET", "/storage/v1/b/second-bucket", "", b"")
            .expect(404);
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
            .request("POST", path, manager, &vec![b' '; 16 * 1024 * 1024 + 1])
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
