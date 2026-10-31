use super::*;

fn full(server: &Server, name: &str) -> Value {
    server
        .request(
            "GET",
            &format!("{BUCKET}/o/{name}?projection=full"),
            ADMIN,
            b"",
        )
        .json()
}

fn acl(value: &Value) -> Vec<(String, String)> {
    let mut entries: Vec<_> = value["acl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["entity"].as_str().unwrap().to_owned(),
                entry["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    entries.sort_unstable();
    entries
}

fn counters(db: &rusqlite::Connection) -> (i64, i64, i64) {
    db.query_row("SELECT (SELECT last_generation FROM object_generation),(SELECT COUNT(*) FROM change_journal),(SELECT COUNT(*) FROM notification_outbox)", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap()
}

fn assert_move_inputs(server: &Server, source: &Value, target: &Value) {
    assert_eq!(full(server, "value"), *source);
    assert_eq!(full(server, "moved"), *target);
    for (name, bytes) in [
        ("value", b"original owner's bytes".as_slice()),
        ("moved", b"old target bytes".as_slice()),
    ] {
        assert_eq!(
            server
                .request("GET", &format!("{BUCKET}/o/{name}?alt=media"), ADMIN, b"")
                .body,
            bytes
        );
    }
}

#[test]
fn move_preserves_source_owner_and_acl_without_observing_current_default() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let mut server = Server::start(&data, &inputs, backend);
        server.bucket();
        super::bucket::grant(&server, "user-reader@example.test", "WRITER");
        super::bucket::grant(&server, "user-other@example.test", "WRITER");
        let source = server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
                READER,
                b"original owner's bytes",
            )
            .json();
        let generation = source["generation"].as_str().unwrap();
        server.grant("user-other@example.test", "OWNER", generation);
        server
            .request(
                "PATCH",
                OBJECT,
                READER,
                br#"{"metadata":{"source":"retained"},"contentType":"text/source"}"#,
            )
            .json();
        let target = server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=moved",
                ADMIN,
                b"old target bytes",
            )
            .json();
        let source = full(&server, "value");
        let before = full(&server, "moved");
        let defaults: Vec<_> = (0..100).map(|index| json!({"entity":format!("user-default{index}@example.test"),"role":"READER"})).collect();
        server
            .admin("PATCH", BUCKET, json!({"defaultObjectAcl":defaults}))
            .json();
        server.admin("POST", &format!("{BUCKET}/notificationConfigs"), json!({"topic":"projects/local/topics/moves","event_types":["OBJECT_DELETE","OBJECT_FINALIZE"]})).json();
        let path = format!("{OBJECT}/moveTo/o/moved?projection=noAcl");
        let db = (backend == "disk")
            .then(|| rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap());
        let prior = db.as_ref().map(counters);
        assert_eq!(
            server
                .request(
                    "POST",
                    &format!("{path}&ifSourceGenerationMatch=0"),
                    OTHER,
                    b""
                )
                .status,
            412
        );
        assert_eq!(
            server
                .request("POST", &format!("{path}&ifGenerationMatch=0"), OTHER, b"")
                .status,
            412
        );
        assert_move_inputs(&server, &source, &before);
        assert_eq!(db.as_ref().map(counters), prior);
        if let Some(db) = &db {
            db.execute_batch("CREATE TRIGGER reject_moved_acl BEFORE INSERT ON acl_object_entries WHEN NEW.name='moved' BEGIN SELECT RAISE(ABORT,'injected moved ACL failure'); END;").unwrap();
            assert_eq!(server.request("POST", &path, OTHER, b"").status, 503);
            db.execute_batch("DROP TRIGGER reject_moved_acl;").unwrap();
            assert_eq!(Some(counters(db)), prior);
            assert_move_inputs(&server, &source, &before);
        }
        server
            .request(
                "POST",
                &format!(
                    "{path}&ifSourceGenerationMatch={generation}&ifGenerationMatch={}",
                    target["generation"].as_str().unwrap()
                ),
                OTHER,
                b"",
            )
            .json();
        let moved = full(&server, "moved");
        assert_eq!(moved["owner"], json!({"entity":"user-reader@example.test"}));
        assert_eq!(acl(&moved), acl(&source));
        assert_eq!(moved["metadata"], source["metadata"]);
        assert_eq!(moved["contentType"], source["contentType"]);
        assert_ne!(moved["generation"], source["generation"]);
        assert_eq!(moved["metageneration"], "1");
        assert_eq!(server.request("GET", OBJECT, ADMIN, b"").status, 404);
        assert_eq!(
            server
                .request("GET", &format!("{BUCKET}/o/moved?alt=media"), READER, b"")
                .body,
            b"original owner's bytes"
        );
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!(
                        "{BUCKET}/o/moved?generation={}",
                        target["generation"].as_str().unwrap()
                    ),
                    OTHER,
                    b""
                )
                .status,
            403
        );
        if let Some(db) = db {
            let events: Vec<String> = db.prepare("SELECT json_extract(message,'$.messages[0].attributes.eventType') FROM notification_outbox ORDER BY id").unwrap().query_map([], |row| row.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
            assert_eq!(events, ["OBJECT_DELETE", "OBJECT_FINALIZE"]);
            drop(db);
            server.stop();
            server = Server::start(&data, &inputs, backend);
            assert_eq!(full(&server, "moved"), moved);
            assert_eq!(
                server
                    .request("GET", &format!("{BUCKET}/o/moved?alt=media"), READER, b"")
                    .body,
                b"original owner's bytes"
            );
        }
    }
}

#[test]
fn move_preserves_legacy_virtual_acl_and_unknown_owner_independently_of_current_default() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut server = Server::start(&data, &inputs, "disk");
    server.bucket();
    server.put(b"legacy source bytes");
    server.stop();
    {
        let db = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        db.execute_batch("UPDATE objects SET owner=NULL WHERE name='value'; DELETE FROM acl_object_entries WHERE name='value';").unwrap();
    }
    server = Server::start(&data, &inputs, "disk");
    let source = full(&server, "value");
    assert!(source.get("owner").is_none());
    let defaults: Vec<_> = (0..100)
        .map(|index| json!({"entity":format!("user-other{index}@example.test"),"role":"READER"}))
        .collect();
    server
        .admin("PATCH", BUCKET, json!({"defaultObjectAcl":defaults}))
        .json();
    server
        .request(
            "POST",
            &format!("{OBJECT}/moveTo/o/legacy?projection=noAcl"),
            ADMIN,
            b"",
        )
        .json();
    let moved = full(&server, "legacy");
    assert!(moved.get("owner").is_none());
    assert_eq!(acl(&moved), acl(&source));
    assert_eq!(acl(&moved).len(), 3);
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/o/legacy?alt=media"), ADMIN, b"")
            .body,
        b"legacy source bytes"
    );
}

#[test]
fn derived_publications_assign_the_caller_and_restore_keeps_original_owner_with_new_default_acl() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        super::bucket::grant(&server, "user-reader@example.test", "WRITER");
        let source = server.put(b"derived bytes");
        server.grant(
            "user-reader@example.test",
            "READER",
            source["generation"].as_str().unwrap(),
        );
        server
            .request(
                "POST",
                &format!("{OBJECT}/copyTo/b/test-bucket/o/copied"),
                READER,
                b"{}",
            )
            .json();
        let rewritten = server
            .request(
                "POST",
                &format!("{OBJECT}/rewriteTo/b/test-bucket/o/rewritten"),
                READER,
                b"{}",
            )
            .json();
        assert_eq!(rewritten["done"], true);
        server
            .request(
                "POST",
                &format!("{BUCKET}/o/composed/compose"),
                READER,
                json!({"sourceObjects":[{"name":"value","generation":source["generation"]}]})
                    .to_string()
                    .as_bytes(),
            )
            .json();
        for name in ["copied", "rewritten", "composed"] {
            let record = full(&server, name);
            assert_eq!(
                record["owner"],
                json!({"entity":"user-reader@example.test"})
            );
            assert_eq!(
                server
                    .request("GET", &format!("{BUCKET}/o/{name}?alt=media"), READER, b"")
                    .body,
                b"derived bytes"
            );
        }
        assert_eq!(
            full(&server, "value")["owner"],
            json!({"entity":"user-admin@example.test"})
        );
        let original = server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=restored",
                READER,
                b"restored owner's bytes",
            )
            .json();
        let generation = original["generation"].as_str().unwrap();
        server
            .admin(
                "POST",
                &format!("{BUCKET}/o/restored/acl"),
                json!({"entity":"user-other@example.test","role":"OWNER"}),
            )
            .json();
        server.admin("PATCH", BUCKET, json!({"softDeletePolicy":{"retentionDurationSeconds":"604800"},"defaultObjectAcl":[{"entity":"user-other@example.test","role":"READER"}]})).json();
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{BUCKET}/o/restored?generation={generation}"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{BUCKET}/acl/user-reader%40example.test"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        let path = format!("{BUCKET}/o/restored/restore?generation={generation}");
        assert_eq!(server.request("POST", &path, READER, b"").status, 403);
        let restored = server.request("POST", &path, ADMIN, b"").json();
        assert_ne!(restored["generation"], original["generation"]);
        let record = full(&server, "restored");
        assert_eq!(
            record["owner"],
            json!({"entity":"user-reader@example.test"})
        );
        assert_eq!(
            acl(&record),
            vec![
                ("user-other@example.test".into(), "READER".into()),
                ("user-reader@example.test".into(), "OWNER".into())
            ]
        );
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!("{BUCKET}/o/restored?alt=media"),
                    READER,
                    b""
                )
                .body,
            b"restored owner's bytes"
        );
    }
}
