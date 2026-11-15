use super::*;

/// The fields that must agree between a startup seed and a seed-defaults fixture of one file;
/// names, generations and times legitimately differ.
const SEED_PARITY_FIELDS: [&str; 6] = [
    "contentType",
    "owner",
    "storageClass",
    "size",
    "md5Hash",
    "crc32c",
];

/// The ACL as entity and role pairs; entry IDs and links embed the object name.
fn acl_grants(resource: &Value) -> Vec<(Value, Value)> {
    resource["acl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| (entry["entity"].clone(), entry["role"].clone()))
        .collect()
}

/// A runtime helper that loads a directory must create what a startup seed of it creates, in
/// both authentication profiles: the extension names the type over the media part's, and the
/// project owners own the object, as no imported identity is attached.
#[test]
fn seed_defaults_publish_the_object_a_startup_seed_publishes() {
    const CSV: &[u8] = b"name,count\nseed,1\n";
    for strict in [false, true] {
        let (data, source) = (TestDir::new(), TestDir::new());
        std::fs::create_dir_all(source.0.join("test-bucket/seeded")).unwrap();
        std::fs::write(source.0.join("test-bucket/seeded/report.csv"), CSV).unwrap();
        let registry = common::process::write_identities(
            &data.0,
            &json!({"identities":[
                {"id":"manager@example.invalid", "tokens":["manager"], "project_roles":{}},
                {"id":"reader@example.invalid", "tokens":["reader"], "project_roles":{"local":["roles/storage.admin"]}}
            ]}),
        );
        let mut options = vec![
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
        ];
        let (manager, reader) = if strict {
            options.extend([
                "--admin-principal",
                "manager@example.invalid",
                "--auth-mode",
                "strict",
                "--identity-file",
                registry.to_str().unwrap(),
            ]);
            (
                "Authorization: Bearer manager\r\n",
                "Authorization: Bearer reader\r\n",
            )
        } else {
            ("", "")
        };
        let server = Server::start(&data, &options);
        let read = |name: &str| {
            server
                .request(
                    "GET",
                    &format!("/storage/v1/b/test-bucket/o/{name}?projection=full"),
                    reader,
                    b"",
                )
                .json_ok()
        };
        server
            .fixture(
                json!({"bucket":"test-bucket","name":"posted/report.csv","defaults":"seed"}),
                CSV,
                manager,
            )
            .json_ok();
        let seeded = read("seeded%2Freport.csv");
        let posted = read("posted%2Freport.csv");
        // Both differ from what a plain fixture gets, so equality below is not vacuous.
        assert_ne!(seeded["contentType"], "application/octet-stream");
        assert!(seeded["owner"]["entity"].is_string(), "{seeded}");
        for field in SEED_PARITY_FIELDS {
            assert_eq!(posted[field], seeded[field], "{field} (strict: {strict})");
        }
        assert_eq!(acl_grants(&posted), acl_grants(&seeded), "strict: {strict}");
        let explicit = server
            .fixture(
                json!({"bucket":"test-bucket","name":"posted/explicit.csv",
                    "defaults":"seed","contentType":"text/plain"}),
                CSV,
                manager,
            )
            .json_ok();
        assert_eq!(explicit["contentType"], "text/plain");
    }
}

/// Without seed defaults a fixture keeps its media type and has no owner, and seed defaults
/// refuse an explicit identity instead of replacing it, creating nothing.
#[test]
fn seed_defaults_are_opt_in_and_refuse_an_explicit_identity() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    for defaults in [json!(null), json!("fixture")] {
        let mut metadata = json!({"bucket":"test-bucket","name":"report.csv"});
        if !defaults.is_null() {
            metadata["defaults"] = defaults;
        }
        let resource = server.fixture(metadata, b"bytes", "").json_ok();
        assert_eq!(resource["contentType"], "application/octet-stream");
        let full = server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/report.csv?projection=full",
                "",
                b"",
            )
            .json_ok();
        assert_eq!(full["owner"], Value::Null);
    }
    for (field, value) in [
        ("generation", json!("7")),
        ("created", json!("2026-01-01T00:00:00Z")),
        ("timeCreated", json!("2026-01-01T00:00:00Z")),
        ("updated", json!("2026-01-01T00:00:00Z")),
    ] {
        let mut metadata = json!({"bucket":"test-bucket","name":"refused.csv","defaults":"seed"});
        metadata[field] = value;
        let message =
            server.fixture(metadata, b"bytes", "").expect(400).json()["error"]["message"].clone();
        assert!(
            message.as_str().is_some_and(|text| text.contains(field)),
            "{field}: {message}"
        );
    }
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"refused.csv","defaults":"startup"}),
            b"bytes",
            "",
        )
        .expect(400);
    server
        .request("GET", "/storage/v1/b/test-bucket/o/refused.csv", "", b"")
        .expect(404);
}

#[test]
fn fixture_publications_past_every_slot_wait_their_turn_and_all_succeed() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let replies = std::thread::scope(|scope| {
        // Twice the 32 callers the management line once held before refusing the next one.
        let publications: Vec<_> = (0..64)
            .map(|index| {
                let server = &server;
                scope.spawn(move || {
                    let metadata =
                        json!({"bucket":"test-bucket","name":format!("fixture-{index}")});
                    server.fixture(metadata, b"fixture bytes", "")
                })
            })
            .collect();
        publications
            .into_iter()
            .map(|publication| publication.join().unwrap())
            .collect::<Vec<_>>()
    });
    for reply in replies {
        reply.json_ok();
    }
}

#[test]
fn direct_fixture_acl_holds_and_retention_use_shared_atomic_validation() {
    let dir = TestDir::new();
    let options = [
        "--admin-enabled",
        "true",
        "--api-base-url",
        "http://fixture.invalid",
    ];
    let server = Server::start(&dir, &options);
    server.populate();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/notificationConfigs",
            "",
            br#"{"topic":"projects/local/topics/events"}"#,
        )
        .json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let notifications = || common::catalog::count(&connection, "notification_outbox");
    let acl = json!([{"entity":"allUsers","role":"READER"}]);
    let resource = server
        .fixture(
            json!({"bucket":"test-bucket","name":"value","acl":acl,
        "temporaryHold":true, "eventBasedHold":true}),
            b"protected",
            "",
        )
        .json_ok();
    assert_eq!(resource["temporaryHold"], true);
    assert_eq!(resource["eventBasedHold"], true);
    let full = server
        .request("GET", &format!("{OBJECT}?projection=full"), "", b"")
        .json_ok();
    assert_eq!(full["acl"][0]["entity"], "allUsers");
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value"}),
            b"blocked",
            "",
        )
        .expect(403);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"protected"
    );
    server
        .request(
            "PATCH",
            OBJECT,
            "",
            br#"{"temporaryHold":false,"eventBasedHold":false}"#,
        )
        .json_ok();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            "",
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .json_ok();
    let before = server.request("GET", OBJECT, "", b"").json_ok();
    let queued = notifications();
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value","acl":acl}),
            b"denied",
            "",
        )
        .expect(403);
    assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), before);
    assert_eq!(notifications(), queued);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            "",
            br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#,
        )
        .json_ok();
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value","acl":[]}),
            b"denied",
            "",
        )
        .expect(400);
    assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), before);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local&enableObjectRetention=true",
            "",
            br#"{"name":"retained-bucket"}"#,
        )
        .json_ok();
    let retained = server
        .fixture(
            json!({"bucket":"retained-bucket","name":"value", "retention":{
        "mode":"Locked", "retainUntilTime":"2090-01-01T00:00:00Z"}}),
            b"retained",
            "",
        )
        .json_ok();
    assert_eq!(retained["retention"]["mode"], "Locked");
    server
        .fixture(
            json!({"bucket":"retained-bucket","name":"value"}),
            b"denied",
            "",
        )
        .expect(403);
    drop(server);
    let server = Server::start(&dir, &options);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/retained-bucket/o/value", "", b"")
            .json_ok(),
        retained
    );
}
