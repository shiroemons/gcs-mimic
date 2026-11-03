use crate::common;
use common::TestDir;
use common::effects::{Reply, Server, request, settings};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Barrier},
    thread,
};

const AUTH: &str = "Authorization: Bearer admin\r\n";

fn setup(strict: bool) -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, strict);
    server.bucket();
    (dir, server)
}

fn compose(server: &Server, name: &str, body: Value) -> Reply {
    server.request(
        "POST",
        &format!("/storage/v1/b/test-bucket/o/{name}/compose"),
        AUTH,
        body.to_string().as_bytes(),
    )
}

fn sources(names: &[&str]) -> Value {
    json!({"sourceObjects": names.iter().map(|name| json!({"name": name})).collect::<Vec<_>>()})
}

fn media(server: &Server, name: &str) -> Reply {
    server.request(
        "GET",
        &format!("/storage/v1/b/test-bucket/o/{name}?alt=media"),
        AUTH,
        b"",
    )
}

#[test]
fn compose_keeps_an_explicit_false_hold_and_ignores_null_custom_time() {
    let (_dir, server) = setup(false);
    server.put("a", b"first").json_ok();
    server.put("b", b"second").json_ok();
    for destination in [json!({"temporaryHold":false}), json!({"customTime":null})] {
        let result = compose(
            &server,
            "target",
            json!({"sourceObjects":[{"name":"a"},{"name":"b"}],"destination":destination}),
        )
        .json_ok();
        assert_eq!(result["componentCount"], 2);
        // An explicit false is kept as sent; the recording does not cover a create-time false, so
        // echoing the request is a guess that adds no state the client did not name.
        if destination.get("temporaryHold").is_some() {
            assert_eq!(result["temporaryHold"], false);
        } else {
            assert!(result.get("temporaryHold").is_none());
        }
        assert!(result.get("customTime").is_none());
        assert_eq!(media(&server, "target").body, b"firstsecond");
    }
}

#[test]
fn compose_order_checksums_metadata_fields_and_restart() {
    let (dir, mut server) = setup(false);
    let first = server.put("a", b"1234").json_ok();
    server.put("b", b"56789").json_ok();
    let body = json!({"sourceObjects": [{"name":"a"}, {"name":"b"}], "destination": {
        "contentType":"application/x-composite", "metadata":{"key":"value"}, "contentLanguage":"en",
        "md5Hash":"ignored", "crc32c":"ignored"
    }});
    let result = compose(&server, "dir%2F%E9%9B%AA%25", body).json_ok();
    assert_eq!(result["name"], "dir/雪%");
    assert_eq!(result["size"], "9");
    assert_eq!(result["componentCount"], 2);
    assert_eq!(result["crc32c"], "4waSgw==");
    assert!(result.get("md5Hash").is_none());
    assert_eq!(result["contentType"], "application/x-composite");
    assert_eq!(result["metadata"]["key"], "value");
    assert_eq!(result["metageneration"], "1");
    assert_ne!(result["generation"], first["generation"]);
    assert!(result.get("owner").is_none() && result.get("acl").is_none());
    let bytes = media(&server, "dir%2F%E9%9B%AA%25");
    assert_eq!(bytes.body, b"123456789");
    assert_eq!(bytes.headers["x-goog-hash"], "crc32c=4waSgw==");
    let xml = server.request("GET", "/test-bucket/dir%2F%E9%9B%AA%25", "", b"");
    xml.expect(200);
    assert_eq!(xml.body, b"123456789");
    assert!(!xml.headers["x-goog-hash"].contains("md5="));
    // XML entity tags are quoted; a composite object has no MD5 to report.
    assert_eq!(
        xml.headers["etag"],
        format!("\"{}\"", result["etag"].as_str().unwrap())
    );
    assert_eq!(xml.headers["x-goog-component-count"], "2");
    let head = server.request("HEAD", "/test-bucket/dir%2F%E9%9B%AA%25", "", b"");
    head.expect(200);
    assert_eq!(head.headers["x-goog-component-count"], "2");
    assert_eq!(head.headers["etag"], xml.headers["etag"]);
    assert!(head.body.is_empty());
    assert!(
        !server
            .request("GET", "/test-bucket/a", "", b"")
            .headers
            .contains_key("x-goog-component-count")
    );
    let selected = server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o/dir%2F%E9%9B%AA%25?fields=componentCount,crc32c,md5Hash",
            AUTH,
            b"",
        )
        .json_ok();
    assert_eq!(selected, json!({"componentCount":2,"crc32c":"4waSgw=="}));
    let listed = server.list("fields=items(name,componentCount,md5Hash)");
    let listed = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "dir/雪%")
        .unwrap();
    assert_eq!(listed["componentCount"], 2);
    assert!(listed.get("md5Hash").is_none());
    let updated = server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/dir%2F%E9%9B%AA%25",
            AUTH,
            br#"{"metadata":{"changed":"yes"}}"#,
        )
        .json_ok();
    assert_eq!(updated["componentCount"], 2);
    assert!(updated.get("md5Hash").is_none());
    let updated_xml = server.request("GET", "/test-bucket/dir%2F%E9%9B%AA%25", "", b"");
    assert_eq!(
        updated_xml.headers["etag"],
        format!("\"{}\"", updated["etag"].as_str().unwrap())
    );
    assert_ne!(updated_xml.headers["etag"], xml.headers["etag"]);
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("dir%2F%E9%9B%AA%25").json_ok(), updated);
    assert_eq!(media(&server, "dir%2F%E9%9B%AA%25").body, b"123456789");
    assert_eq!(server.get("a").json_ok(), first);
}

#[test]
fn compose_one_empty_repeated_and_nested_sources_exceed_old_component_limit() {
    let (_dir, server) = setup(false);
    server.put("empty", b"").json_ok();
    server.put("a", b"a").json_ok();
    let empty = compose(&server, "empty-composite", sources(&["empty"])).json_ok();
    assert_eq!(empty["componentCount"], 1);
    assert_eq!(empty["crc32c"], "AAAAAA==");
    assert!(empty.get("md5Hash").is_none());
    assert!(media(&server, "empty-composite").body.is_empty());
    let mut count = 1;
    let mut source = "a".to_owned();
    for index in 0..3 {
        let name = format!("nested-{index}");
        let body = json!({"sourceObjects": vec![json!({"name":source}); 32]});
        let object = compose(&server, &name, body).json_ok();
        count *= 32;
        assert_eq!(object["componentCount"], count);
        assert_eq!(object["size"], count.to_string());
        source = name;
    }
    assert_eq!(media(&server, &source).body, vec![b'a'; count]);
    let uploaded = server.put(&source, b"ordinary").json_ok();
    assert!(uploaded.get("componentCount").is_none());
    assert!(uploaded.get("md5Hash").is_some());
}

#[test]
fn compose_source_and_destination_preconditions_preserve_failed_overwrites() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let first = server.put("source", b"old").json_ok();
    let next = server.put("source", b"new").json_ok();
    let old_destination = server.put("destination", b"preserved").json_ok();
    for body in [
        sources(&["missing"]),
        json!({"sourceObjects":[{"name":"source","generation":"1"}]}),
        json!({"sourceObjects":[{"name":"source","objectPreconditions":{"ifGenerationMatch": first["generation"]}}]}),
        json!({"sourceObjects":[{"name":"source","generation": first["generation"],"objectPreconditions":{"ifGenerationMatch":next["generation"]}}]}),
    ] {
        let reply = compose(&server, "destination", body);
        assert!(matches!(reply.status, 404 | 412));
        assert_eq!(server.get("destination").json_ok(), old_destination);
    }
    for query in [
        "ifGenerationMatch=0",
        "ifGenerationMatch=1",
        "ifMetagenerationMatch=2",
    ] {
        let reply = server.request(
            "POST",
            &format!("/storage/v1/b/test-bucket/o/destination/compose?{query}"),
            AUTH,
            sources(&["source"]).to_string().as_bytes(),
        );
        reply.json_reason(412, "conditionNotMet");
        assert_eq!(server.get("destination").json_ok(), old_destination);
    }
    let pinned = compose(&server, "destination", json!({"sourceObjects":[{"name":"source", "generation":first["generation"], "objectPreconditions":{"ifGenerationMatch":first["generation"]}}]})).json_ok();
    assert_eq!(media(&server, "destination").body, b"old");
    assert_ne!(pinned["generation"], old_destination["generation"]);
    assert_eq!(
        server
            .get(&format!(
                "destination?generation={}",
                old_destination["generation"].as_str().unwrap()
            ))
            .json_ok()["size"],
        "9"
    );
}

#[test]
fn compose_hard_deletion_is_atomic_deduplicated_and_preserves_new_destination() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let a_old = server.put("a", b"old").json_ok();
    let a = server.put("a", b"a").json_ok();
    let b = server.put("b", b"b").json_ok();
    let destination = server.put("destination", b"d").json_ok();
    let held = server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/b",
            AUTH,
            br#"{"eventBasedHold":true}"#,
        )
        .json_ok();
    let body = json!({"sourceObjects":[{"name":"a"},{"name":"a"},{"name":"b"},{"name":"destination"}],"deleteSourceObjects":true});
    compose(&server, "destination", body.clone()).hold_error_reason();
    assert_eq!(server.get("destination").json_ok(), destination);
    assert_eq!(server.get("a").json_ok(), a);
    assert_eq!(server.get("b").json_ok(), held);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/b",
            AUTH,
            br#"{"eventBasedHold":false}"#,
        )
        .json_ok();
    let mut missing = body.clone();
    missing["sourceObjects"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"missing"}));
    compose(&server, "destination", missing).json_reason(404, "notFound");
    assert_eq!(server.get("a").json_ok(), a);
    let result = compose(&server, "destination", body).json_ok();
    assert_eq!(result["componentCount"], 4);
    assert_eq!(media(&server, "destination").body, b"aabd");
    server.get("a").json_reason(404, "notFound");
    server.get("b").json_reason(404, "notFound");
    server
        .get(&format!(
            "a?generation={}",
            a["generation"].as_str().unwrap()
        ))
        .json_reason(404, "notFound");
    server
        .get(&format!(
            "b?generation={}",
            b["generation"].as_str().unwrap()
        ))
        .json_reason(404, "notFound");
    let retained_destination = server
        .get(&format!(
            "destination?generation={}",
            destination["generation"].as_str().unwrap()
        ))
        .json_ok();
    assert_eq!(retained_destination["size"], "1");
    assert!(retained_destination.get("timeDeleted").is_some());
    assert_eq!(common::effects::blobs(&dir), 3);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        common::catalog::count(&connection, "pending_blob_deletions"),
        0
    );
    assert_eq!(
        server
            .request(
                "GET",
                &format!(
                    "/storage/v1/b/test-bucket/o/a?generation={}&alt=media",
                    a_old["generation"].as_str().unwrap()
                ),
                AUTH,
                b""
            )
            .body,
        b"old"
    );
    let old_source = compose(&server, "selected-old", json!({"sourceObjects":[{"name":"a","generation":a_old["generation"]}],"deleteSourceObjects":true})).json_ok();
    assert_eq!(old_source["componentCount"], 1);
    assert_eq!(media(&server, "selected-old").body, b"old");
    server
        .get(&format!(
            "a?generation={}",
            a_old["generation"].as_str().unwrap()
        ))
        .json_reason(404, "notFound");
}

#[test]
fn compose_rejects_invalid_or_unsupported_input_without_mutation() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json_ok();
    let destination = server.put("destination", b"preserved").json_ok();
    for body in [
        json!({}),
        json!({"sourceObjects":[]}),
        json!({"sourceObjects":vec![json!({"name":"a"});33]}),
        json!({"sourceObjects":[{"name":"a","generation":-1}]}),
    ] {
        compose(&server, "destination", body).expect(400);
    }
    for query in [
        "destinationPredefinedAcl=publicRead",
        "kmsKeyName=key",
        "userProject=billed",
        "dropContextGroups=custom",
        "ifGenerationNotMatch=1",
    ] {
        server
            .request(
                "POST",
                &format!("/storage/v1/b/test-bucket/o/destination/compose?{query}"),
                AUTH,
                sources(&["a"]).to_string().as_bytes(),
            )
            .json_reason(400, "badRequest");
    }
    for destination_body in [
        json!({"acl":[]}),
        json!({"kmsKeyName":"key"}),
        json!({"retention":{}}),
        json!({"contexts":{}}),
        json!({"temporaryHold":true}),
        json!({"customTime":"2026-10-20T01:02:03Z"}),
        json!({"storageClass":"STANDARD"}),
        json!({"storageClass":"NEARLINE"}),
    ] {
        compose(
            &server,
            "destination",
            json!({"sourceObjects":[{"name":"a"}],"destination":destination_body}),
        )
        .json_reason(400, "badRequest");
    }
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose?fields=missingField",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json_reason(400, "invalidParameter");
    assert_eq!(server.get("destination").json_ok(), destination);
}

#[test]
fn compose_strict_requires_both_read_create_and_delete_for_destructive_cases() {
    let (_dir, server) = setup(true);
    server.put("a", b"a").json_ok();
    for (token, status, reason) in [
        ("", 401, "required"),
        ("viewer", 403, "forbidden"),
        ("creator", 403, "forbidden"),
    ] {
        let auth = if token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {token}\r\n")
        };
        server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/destination/compose",
                &auth,
                sources(&["a"]).to_string().as_bytes(),
            )
            .json_reason(status, reason);
    }
    server.request("PUT", "/storage/v1/b/test-bucket/iam", AUTH, br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["user:creator@example.test"]}]}"#).json_ok();
    let auth = "Authorization: Bearer creator\r\n";
    let created = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            auth,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            auth,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json_reason(403, "forbidden");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/deleted/compose",
            auth,
            br#"{"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}"#,
        )
        .json_reason(403, "forbidden");
    assert_eq!(server.get("destination").json_ok(), created);
    assert_eq!(media(&server, "a").body, b"a");
}

#[test]
fn compose_concurrent_create_if_absent_has_one_winner() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json_ok();
    let barrier = Arc::new(Barrier::new(8));
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let barrier = barrier.clone();
            let address = server.address.clone();
            thread::spawn(move || {
                barrier.wait();
                request(
                    &address,
                    "POST",
                    "/storage/v1/b/test-bucket/o/destination/compose?ifGenerationMatch=0",
                    AUTH,
                    br#"{"sourceObjects":[{"name":"a"}]}"#,
                )
                .status
            })
        })
        .collect();
    let statuses: Vec<_> = requests
        .into_iter()
        .map(|request| request.join().unwrap())
        .collect();
    assert_eq!(statuses.iter().filter(|status| **status == 200).count(), 1);
    assert_eq!(statuses.iter().filter(|status| **status == 412).count(), 7);
    assert_eq!(media(&server, "destination").body, b"a");
}

#[test]
fn compose_storage_class_defaults_and_event_holds() {
    let (dir, server) = setup(false);
    server.put("standard", b"s").json_ok();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            AUTH,
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json_ok();
    server.put("nearline", b"n").json_ok();
    let same_class = compose(&server, "same-class", sources(&["standard"])).json_ok();
    assert_eq!(same_class["storageClass"], "STANDARD");
    let mixed = compose(&server, "mixed", sources(&["standard", "nearline"]));
    mixed.expect(400);
    server.get("mixed").json_reason(404, "notFound");
    settings(&dir, false, true);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/standard",
            AUTH,
            br#"{"eventBasedHold":true}"#,
        )
        .json_ok();
    let held = compose(&server, "held", sources(&["standard"])).json_ok();
    assert_eq!(held["eventBasedHold"], true);
    compose(&server, "held", sources(&["standard"])).hold_error_reason();
    let released = compose(
        &server,
        "released",
        json!({"sourceObjects":[{"name":"standard"}],"destination":{"eventBasedHold":false}}),
    )
    .json_ok();
    assert_eq!(released["eventBasedHold"], false);
}

#[test]
fn compose_method_body_and_selector_boundaries() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json_ok();
    let method = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/destination/compose",
        AUTH,
        b"",
    );
    method.json_reason(405, "methodNotAllowed");
    assert_eq!(method.headers["allow"], "POST");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            AUTH,
            b"{",
        )
        .json_reason(400, "invalid");
    let oversized = vec![b' '; 65537];
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            AUTH,
            &oversized,
        )
        .expect(413);
    for query in ["projection=full", "projection=noAcl"] {
        server
            .request(
                "POST",
                &format!("/storage/v1/b/test-bucket/o/destination/compose?{query}"),
                AUTH,
                sources(&["a"]).to_string().as_bytes(),
            )
            .json_reason(400, "invalidParameter");
        server.get("destination").json_reason(404, "notFound");
    }
    let selected = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose?fields=name,componentCount,md5Hash",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json_ok();
    assert_eq!(selected, json!({"name":"destination","componentCount":1}));
}

#[test]
fn compose_rechecks_read_and_create_grants_after_streaming() {
    let (dir, server) = setup(true);
    server.put("a", b"source").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // Revoke at the durable checkpoint to exercise publication checks without timing races.
    connection.execute_batch("CREATE TRIGGER revoke_compose_grant AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE bucket_policies SET bindings = '[]' WHERE bucket = 'test-bucket'; END;").unwrap();
    for (token, role) in [
        ("creator", "roles/storage.objectViewer"),
        ("viewer", "roles/storage.objectCreator"),
    ] {
        server.request("PUT", "/storage/v1/b/test-bucket/iam", AUTH, json!({"bindings":[{"role":role,"members":[format!("user:{token}@example.test")]}]}).to_string().as_bytes()).json_ok();
        server
            .request(
                "POST",
                &format!("/storage/v1/b/test-bucket/o/revoked-{token}/compose"),
                &format!("Authorization: Bearer {token}\r\n"),
                sources(&["a"]).to_string().as_bytes(),
            )
            .json_reason(403, "forbidden");
        server
            .get(&format!("revoked-{token}"))
            .json_reason(404, "notFound");
        assert_eq!(media(&server, "a").body, b"source");
    }
    connection
        .execute_batch("DROP TRIGGER revoke_compose_grant;")
        .unwrap();
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn compose_late_source_delete_failure_rolls_back_destination_and_earlier_deletions() {
    let (dir, server) = setup(false);
    let a = server.put("a", b"first").json_ok();
    let b = server.put("b", b"second").json_ok();
    let destination = server.put("destination", b"preserved").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // Fail after the destination insert and the earlier source deletion have executed.
    connection.execute_batch("CREATE TRIGGER fail_compose_delete BEFORE DELETE ON objects WHEN OLD.name = 'b' BEGIN SELECT RAISE(ABORT, 'injected compose delete failure'); END;").unwrap();
    compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"a"},{"name":"b"}],"deleteSourceObjects":true}),
    )
    .json_reason(503, "backendError");
    connection
        .execute_batch("DROP TRIGGER fail_compose_delete;")
        .unwrap();
    assert_eq!(server.get("destination").json_ok(), destination);
    assert_eq!(server.get("a").json_ok(), a);
    assert_eq!(server.get("b").json_ok(), b);
    assert_eq!(media(&server, "destination").body, b"preserved");
    assert_eq!(common::process::staging_count(&dir), 0);
    assert_eq!(
        common::catalog::count(&connection, "pending_blob_deletions"),
        0
    );
}

#[test]
fn compose_strict_admin_can_replace_self_source_and_delete_other_sources() {
    let (_dir, server) = setup(true);
    let first = server.put("destination", b"a").json_ok();
    server.put("b", b"b").json_ok();
    let result = compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"destination"},{"name":"b"}],"deleteSourceObjects":true}),
    )
    .json_ok();
    assert_ne!(result["generation"], first["generation"]);
    assert_eq!(media(&server, "destination").body, b"ab");
    server.get("b").json_reason(404, "notFound");
    server
        .get(&format!(
            "destination?generation={}",
            first["generation"].as_str().unwrap()
        ))
        .json_reason(404, "notFound");
}

#[test]
fn compose_hold_added_after_streaming_preserves_destination_and_sources() {
    let (dir, server) = setup(false);
    server.put("a", b"source").json_ok();
    let destination = server.put("destination", b"preserved").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER hold_compose_source AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE objects SET event_based_hold = 1 WHERE name = 'a'; END;").unwrap();
    compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}),
    )
    .hold_error_reason();
    connection
        .execute_batch("DROP TRIGGER hold_compose_source;")
        .unwrap();
    assert_eq!(server.get("a").json_ok()["eventBasedHold"], true);
    assert_eq!(media(&server, "a").body, b"source");
    assert_eq!(server.get("destination").json_ok(), destination);
    assert_eq!(common::catalog::count(&connection, "derived_transfers"), 0);
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn compose_source_generation_change_never_deletes_the_replacement() {
    let (dir, server) = setup(false);
    let source = server.put("a", b"source").json_ok();
    let generation: i64 = source["generation"].as_str().unwrap().parse().unwrap();
    let destination = server.put("destination", b"preserved").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON; SAVEPOINT generation_only;")
        .unwrap();
    let error = connection.execute("UPDATE objects SET generation = generation + 1 WHERE bucket = 'test-bucket' AND name = 'a'", []).unwrap_err();
    assert!(
        matches!(&error, rusqlite::Error::SqliteFailure(cause, _) if cause.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
        "{error}"
    );
    connection
        .execute_batch("ROLLBACK TO generation_only; RELEASE generation_only;")
        .unwrap();
    // Replacement must move the generation-bound ACL with its parent rather than corrupt its FK.
    connection.execute_batch("CREATE TABLE compose_source_acl AS SELECT bucket, name, entries FROM acl_object_entries WHERE bucket = 'test-bucket' AND name = 'a';
        CREATE TRIGGER replace_compose_source AFTER UPDATE OF offset ON derived_transfers BEGIN
        DELETE FROM acl_object_entries WHERE bucket = 'test-bucket' AND name = 'a';
        UPDATE objects SET generation = generation + 1 WHERE bucket = 'test-bucket' AND name = 'a';
        INSERT INTO acl_object_entries SELECT o.bucket, o.name, o.generation, a.entries FROM objects o JOIN compose_source_acl a ON o.bucket = a.bucket AND o.name = a.name;
        UPDATE object_generation SET last_generation = max(last_generation, (SELECT max(generation) FROM objects)); END;").unwrap();
    compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}),
    )
    .json_reason(404, "notFound");
    connection
        .execute_batch("DROP TRIGGER replace_compose_source; DROP TABLE compose_source_acl;")
        .unwrap();
    assert!(
        !connection
            .prepare("PRAGMA foreign_key_check")
            .unwrap()
            .exists([])
            .unwrap()
    );
    assert_eq!(
        server.get("a").json_ok()["generation"],
        (generation + 1).to_string()
    );
    assert_eq!(media(&server, "a").body, b"source");
    assert_eq!(server.get("destination").json_ok(), destination);
    assert_eq!(common::catalog::count(&connection, "derived_transfers"), 0);
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn compose_destination_condition_is_rechecked_after_streaming() {
    let (dir, server) = setup(false);
    server.put("a", b"source").json_ok();
    let destination = server.put("destination", b"preserved").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER change_compose_destination AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE objects SET metageneration = metageneration + 1 WHERE name = 'destination'; END;").unwrap();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose?ifMetagenerationMatch=1",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json_reason(412, "conditionNotMet");
    connection
        .execute_batch("DROP TRIGGER change_compose_destination;")
        .unwrap();
    let after = server.get("destination").json_ok();
    assert_eq!(after["generation"], destination["generation"]);
    assert_eq!(after["metageneration"], "2");
    assert_eq!(media(&server, "destination").body, b"preserved");
    assert_eq!(common::catalog::count(&connection, "derived_transfers"), 0);
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn compose_incarnation_change_preserves_original_error_and_recovers_after_restart() {
    let (dir, mut server) = setup(false);
    let source = server.put("a", b"source").json_ok();
    let destination = server.put("destination", b"preserved").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER replace_compose_bucket AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE bucket_incarnations SET incarnation = CASE WHEN incarnation = '00000000000000000000000000000000' THEN '11111111111111111111111111111111' ELSE '00000000000000000000000000000000' END WHERE bucket = 'test-bucket'; END;
        CREATE TRIGGER fail_compose_cleanup BEFORE DELETE ON derived_transfers BEGIN SELECT RAISE(ABORT, 'injected compose cleanup failure'); END;").unwrap();
    compose(&server, "destination", sources(&["a"])).json_reason(412, "conditionNotMet");
    assert_eq!(server.get("destination").json_ok(), destination);
    assert_eq!(server.get("a").json_ok(), source);
    assert_eq!(common::catalog::count(&connection, "derived_transfers"), 1);
    connection
        .execute_batch("DROP TRIGGER replace_compose_bucket; DROP TRIGGER fail_compose_cleanup;")
        .unwrap();
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("destination").json_ok(), destination);
    assert_eq!(server.get("a").json_ok(), source);
    assert_eq!(common::catalog::count(&connection, "derived_transfers"), 0);
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn compose_rechecks_source_delete_grant_while_read_and_create_remain_allowed() {
    let (dir, server) = setup(true);
    let source = server.put("a", b"source").json_ok();
    server.request("PUT", "/storage/v1/b/test-bucket/iam", AUTH, br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["user:creator@example.test"]},{"role":"roles/storage.objectUser","members":["user:creator@example.test"]}]}"#).json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // Leave read/create allowed so only the source-deletion recheck can reject publication.
    connection.execute_batch("CREATE TRIGGER revoke_compose_delete AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE bucket_policies SET bindings = '[{\"role\":\"roles/storage.objectViewer\",\"members\":[\"user:creator@example.test\"]}]' WHERE bucket = 'test-bucket'; END;").unwrap();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            "Authorization: Bearer creator\r\n",
            br#"{"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}"#,
        )
        .json_reason(403, "forbidden");
    connection
        .execute_batch("DROP TRIGGER revoke_compose_delete;")
        .unwrap();
    server.get("destination").json_reason(404, "notFound");
    assert_eq!(server.get("a").json_ok(), source);
    assert_eq!(common::catalog::count(&connection, "derived_transfers"), 0);
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn compose_same_name_noncurrent_source_hold_is_rechecked_before_publication() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let old = server.put("destination", b"old").json_ok();
    let generation: i64 = old["generation"].as_str().unwrap().parse().unwrap();
    let current = server.put("destination", b"current").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // A held noncurrent self-source bypasses the ordinary live-destination hold check.
    connection.execute_batch(&format!("CREATE TRIGGER hold_compose_self_source AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE objects SET event_based_hold = 1 WHERE name = 'destination' AND generation = {generation}; END;")).unwrap();
    compose(&server,"destination",json!({"sourceObjects":[{"name":"destination","generation":generation.to_string()}],"deleteSourceObjects":true})).hold_error_reason();
    connection
        .execute_batch("DROP TRIGGER hold_compose_self_source;")
        .unwrap();
    assert_eq!(server.get("destination").json_ok(), current);
    assert_eq!(media(&server, "destination").body, b"current");
    assert_eq!(
        server
            .get(&format!("destination?generation={generation}"))
            .json_ok()["eventBasedHold"],
        true
    );
    assert_eq!(common::catalog::count(&connection, "derived_transfers"), 0);
    assert_eq!(common::process::staging_count(&dir), 0);
}
