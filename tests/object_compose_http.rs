mod object_effects_support;
use object_effects_support::{Reply, Server, common::TestDir, request, settings};
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
fn compose_accepts_noop_hold_and_custom_time_defaults() {
    let (_dir, server) = setup(false);
    server.put("a", b"first").json();
    server.put("b", b"second").json();
    for destination in [json!({"temporaryHold":false}), json!({"customTime":null})] {
        let result = compose(
            &server,
            "target",
            json!({"sourceObjects":[{"name":"a"},{"name":"b"}],"destination":destination}),
        )
        .json();
        assert_eq!(result["componentCount"], 2);
        assert!(result.get("temporaryHold").is_none());
        assert!(result.get("customTime").is_none());
        assert_eq!(media(&server, "target").body, b"firstsecond");
    }
}

#[test]
fn compose_order_checksums_metadata_fields_and_restart() {
    let (dir, mut server) = setup(false);
    let first = server.put("a", b"1234").json();
    server.put("b", b"56789").json();
    let body = json!({"sourceObjects": [{"name":"a"}, {"name":"b"}], "destination": {
        "contentType":"application/x-composite", "metadata":{"key":"value"}, "contentLanguage":"en",
        "md5Hash":"ignored", "crc32c":"ignored"
    }});
    let result = compose(&server, "dir%2F%E9%9B%AA%25", body).json();
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
    assert_eq!(xml.status, 200);
    assert_eq!(xml.body, b"123456789");
    assert!(!xml.headers["x-goog-hash"].contains("md5="));
    assert_eq!(xml.headers["etag"], result["etag"].as_str().unwrap());
    assert_eq!(xml.headers["x-goog-component-count"], "2");
    let head = server.request("HEAD", "/test-bucket/dir%2F%E9%9B%AA%25", "", b"");
    assert_eq!(head.status, 200);
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
        .json();
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
        .json();
    assert_eq!(updated["componentCount"], 2);
    assert!(updated.get("md5Hash").is_none());
    let updated_xml = server.request("GET", "/test-bucket/dir%2F%E9%9B%AA%25", "", b"");
    assert_eq!(
        updated_xml.headers["etag"],
        updated["etag"].as_str().unwrap()
    );
    assert_ne!(updated_xml.headers["etag"], xml.headers["etag"]);
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("dir%2F%E9%9B%AA%25").json(), updated);
    assert_eq!(media(&server, "dir%2F%E9%9B%AA%25").body, b"123456789");
    assert_eq!(server.get("a").json(), first);
}

#[test]
fn compose_one_empty_repeated_and_nested_sources_exceed_old_component_limit() {
    let (_dir, server) = setup(false);
    server.put("empty", b"").json();
    server.put("a", b"a").json();
    let empty = compose(&server, "empty-composite", sources(&["empty"])).json();
    assert_eq!(empty["componentCount"], 1);
    assert_eq!(empty["crc32c"], "AAAAAA==");
    assert!(empty.get("md5Hash").is_none());
    assert!(media(&server, "empty-composite").body.is_empty());
    let mut count = 1;
    let mut source = "a".to_owned();
    for index in 0..3 {
        let name = format!("nested-{index}");
        let body = json!({"sourceObjects": vec![json!({"name":source}); 32]});
        let object = compose(&server, &name, body).json();
        count *= 32;
        assert_eq!(object["componentCount"], count);
        assert_eq!(object["size"], count.to_string());
        source = name;
    }
    assert_eq!(media(&server, &source).body, vec![b'a'; count]);
    let uploaded = server.put(&source, b"ordinary").json();
    assert!(uploaded.get("componentCount").is_none());
    assert!(uploaded.get("md5Hash").is_some());
}

#[test]
fn compose_source_and_destination_preconditions_preserve_failed_overwrites() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let first = server.put("source", b"old").json();
    let next = server.put("source", b"new").json();
    let old_destination = server.put("destination", b"preserved").json();
    for body in [
        sources(&["missing"]),
        json!({"sourceObjects":[{"name":"source","generation":"1"}]}),
        json!({"sourceObjects":[{"name":"source","objectPreconditions":{"ifGenerationMatch": first["generation"]}}]}),
        json!({"sourceObjects":[{"name":"source","generation": first["generation"],"objectPreconditions":{"ifGenerationMatch":next["generation"]}}]}),
    ] {
        let reply = compose(&server, "destination", body);
        assert!(matches!(reply.status, 404 | 412));
        assert_eq!(server.get("destination").json(), old_destination);
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
        reply.error(412, "conditionNotMet");
        assert_eq!(server.get("destination").json(), old_destination);
    }
    let pinned = compose(&server, "destination", json!({"sourceObjects":[{"name":"source", "generation":first["generation"], "objectPreconditions":{"ifGenerationMatch":first["generation"]}}]})).json();
    assert_eq!(media(&server, "destination").body, b"old");
    assert_ne!(pinned["generation"], old_destination["generation"]);
    assert_eq!(
        server
            .get(&format!(
                "destination?generation={}",
                old_destination["generation"].as_str().unwrap()
            ))
            .json()["size"],
        "9"
    );
}

#[test]
fn compose_hard_deletion_is_atomic_deduplicated_and_preserves_new_destination() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let a_old = server.put("a", b"old").json();
    let a = server.put("a", b"a").json();
    let b = server.put("b", b"b").json();
    let destination = server.put("destination", b"d").json();
    let held = server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/b",
            AUTH,
            br#"{"eventBasedHold":true}"#,
        )
        .json();
    let body = json!({"sourceObjects":[{"name":"a"},{"name":"a"},{"name":"b"},{"name":"destination"}],"deleteSourceObjects":true});
    compose(&server, "destination", body.clone()).error(403, "objectUnderActiveHold");
    assert_eq!(server.get("destination").json(), destination);
    assert_eq!(server.get("a").json(), a);
    assert_eq!(server.get("b").json(), held);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/b",
            AUTH,
            br#"{"eventBasedHold":false}"#,
        )
        .json();
    let mut missing = body.clone();
    missing["sourceObjects"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"missing"}));
    compose(&server, "destination", missing).error(404, "notFound");
    assert_eq!(server.get("a").json(), a);
    let result = compose(&server, "destination", body).json();
    assert_eq!(result["componentCount"], 4);
    assert_eq!(media(&server, "destination").body, b"aabd");
    server.get("a").error(404, "notFound");
    server.get("b").error(404, "notFound");
    server
        .get(&format!(
            "a?generation={}",
            a["generation"].as_str().unwrap()
        ))
        .error(404, "notFound");
    server
        .get(&format!(
            "b?generation={}",
            b["generation"].as_str().unwrap()
        ))
        .error(404, "notFound");
    let retained_destination = server
        .get(&format!(
            "destination?generation={}",
            destination["generation"].as_str().unwrap()
        ))
        .json();
    assert_eq!(retained_destination["size"], "1");
    assert!(retained_destination.get("timeDeleted").is_some());
    assert_eq!(object_effects_support::blobs(&dir), 3);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
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
    let old_source = compose(&server, "selected-old", json!({"sourceObjects":[{"name":"a","generation":a_old["generation"]}],"deleteSourceObjects":true})).json();
    assert_eq!(old_source["componentCount"], 1);
    assert_eq!(media(&server, "selected-old").body, b"old");
    server
        .get(&format!(
            "a?generation={}",
            a_old["generation"].as_str().unwrap()
        ))
        .error(404, "notFound");
}

#[test]
fn compose_rejects_invalid_or_unsupported_input_without_mutation() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json();
    let destination = server.put("destination", b"preserved").json();
    for body in [
        json!({}),
        json!({"sourceObjects":[]}),
        json!({"sourceObjects":vec![json!({"name":"a"});33]}),
        json!({"sourceObjects":[{"name":"a","generation":-1}]}),
    ] {
        assert_eq!(compose(&server, "destination", body).status, 400);
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
            .error(400, "badRequest");
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
        .error(400, "badRequest");
    }
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose?fields=missingField",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .error(400, "invalidParameter");
    assert_eq!(server.get("destination").json(), destination);
}

#[test]
fn compose_strict_requires_both_read_create_and_delete_for_destructive_cases() {
    let (_dir, server) = setup(true);
    server.put("a", b"a").json();
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
            .error(status, reason);
    }
    server.request("PUT", "/storage/v1/b/test-bucket/iam", AUTH, br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["user:creator@example.test"]}]}"#).json();
    let auth = "Authorization: Bearer creator\r\n";
    let created = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            auth,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            auth,
            sources(&["a"]).to_string().as_bytes(),
        )
        .error(403, "forbidden");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/deleted/compose",
            auth,
            br#"{"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}"#,
        )
        .error(403, "forbidden");
    assert_eq!(server.get("destination").json(), created);
    assert_eq!(media(&server, "a").body, b"a");
}

#[test]
fn compose_concurrent_create_if_absent_has_one_winner() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json();
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
    server.put("standard", b"s").json();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            AUTH,
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json();
    server.put("nearline", b"n").json();
    let same_class = compose(&server, "same-class", sources(&["standard"])).json();
    assert_eq!(same_class["storageClass"], "STANDARD");
    let mixed = compose(&server, "mixed", sources(&["standard", "nearline"]));
    assert_eq!(mixed.status, 400);
    server.get("mixed").error(404, "notFound");
    settings(&dir, false, true);
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/standard",
            AUTH,
            br#"{"eventBasedHold":true}"#,
        )
        .json();
    let held = compose(&server, "held", sources(&["standard"])).json();
    assert_eq!(held["eventBasedHold"], true);
    compose(&server, "held", sources(&["standard"])).error(403, "objectUnderActiveHold");
    let released = compose(
        &server,
        "released",
        json!({"sourceObjects":[{"name":"standard"}],"destination":{"eventBasedHold":false}}),
    )
    .json();
    assert!(released.get("eventBasedHold").is_none());
}

#[test]
fn compose_method_body_and_selector_boundaries() {
    let (_dir, server) = setup(false);
    server.put("a", b"a").json();
    let method = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/destination/compose",
        AUTH,
        b"",
    );
    method.error(405, "methodNotAllowed");
    assert_eq!(method.headers["allow"], "POST");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose",
            AUTH,
            b"{",
        )
        .error(400, "parseError");
    let oversized = vec![b' '; 65537];
    assert_eq!(
        server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/destination/compose",
                AUTH,
                &oversized
            )
            .status,
        413
    );
    for query in ["projection=full", "projection=noAcl", "undeclared=value"] {
        server
            .request(
                "POST",
                &format!("/storage/v1/b/test-bucket/o/destination/compose?{query}"),
                AUTH,
                sources(&["a"]).to_string().as_bytes(),
            )
            .error(400, "invalidParameter");
        server.get("destination").error(404, "notFound");
    }
    let selected = server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose?fields=name,componentCount,md5Hash",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .json();
    assert_eq!(selected, json!({"name":"destination","componentCount":1}));
}

#[test]
fn compose_rechecks_read_and_create_grants_after_streaming() {
    let (dir, server) = setup(true);
    server.put("a", b"source").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // Revoke at the durable checkpoint to exercise publication checks without timing races.
    connection.execute_batch("CREATE TRIGGER revoke_compose_grant AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE bucket_policies SET bindings = '[]' WHERE bucket = 'test-bucket'; END;").unwrap();
    for (token, role) in [
        ("creator", "roles/storage.objectViewer"),
        ("viewer", "roles/storage.objectCreator"),
    ] {
        server.request("PUT", "/storage/v1/b/test-bucket/iam", AUTH, json!({"bindings":[{"role":role,"members":[format!("user:{token}@example.test")]}]}).to_string().as_bytes()).json();
        server
            .request(
                "POST",
                &format!("/storage/v1/b/test-bucket/o/revoked-{token}/compose"),
                &format!("Authorization: Bearer {token}\r\n"),
                sources(&["a"]).to_string().as_bytes(),
            )
            .error(403, "forbidden");
        server
            .get(&format!("revoked-{token}"))
            .error(404, "notFound");
        assert_eq!(media(&server, "a").body, b"source");
    }
    connection
        .execute_batch("DROP TRIGGER revoke_compose_grant;")
        .unwrap();
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn compose_late_source_delete_failure_rolls_back_destination_and_earlier_deletions() {
    let (dir, server) = setup(false);
    let a = server.put("a", b"first").json();
    let b = server.put("b", b"second").json();
    let destination = server.put("destination", b"preserved").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // Fail after the destination insert and the earlier source deletion have executed.
    connection.execute_batch("CREATE TRIGGER fail_compose_delete BEFORE DELETE ON objects WHEN OLD.name = 'b' BEGIN SELECT RAISE(ABORT, 'injected compose delete failure'); END;").unwrap();
    compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"a"},{"name":"b"}],"deleteSourceObjects":true}),
    )
    .error(503, "backendError");
    connection
        .execute_batch("DROP TRIGGER fail_compose_delete;")
        .unwrap();
    assert_eq!(server.get("destination").json(), destination);
    assert_eq!(server.get("a").json(), a);
    assert_eq!(server.get("b").json(), b);
    assert_eq!(media(&server, "destination").body, b"preserved");
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn compose_strict_admin_can_replace_self_source_and_delete_other_sources() {
    let (_dir, server) = setup(true);
    let first = server.put("destination", b"a").json();
    server.put("b", b"b").json();
    let result = compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"destination"},{"name":"b"}],"deleteSourceObjects":true}),
    )
    .json();
    assert_ne!(result["generation"], first["generation"]);
    assert_eq!(media(&server, "destination").body, b"ab");
    server.get("b").error(404, "notFound");
    server
        .get(&format!(
            "destination?generation={}",
            first["generation"].as_str().unwrap()
        ))
        .error(404, "notFound");
}

#[test]
fn compose_hold_added_after_streaming_preserves_destination_and_sources() {
    let (dir, server) = setup(false);
    server.put("a", b"source").json();
    let destination = server.put("destination", b"preserved").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER hold_compose_source AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE objects SET event_based_hold = 1 WHERE name = 'a'; END;").unwrap();
    compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}),
    )
    .error(403, "objectUnderActiveHold");
    connection
        .execute_batch("DROP TRIGGER hold_compose_source;")
        .unwrap();
    assert_eq!(server.get("a").json()["eventBasedHold"], true);
    assert_eq!(media(&server, "a").body, b"source");
    assert_eq!(server.get("destination").json(), destination);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn compose_source_generation_change_never_deletes_the_replacement() {
    let (dir, server) = setup(false);
    let source = server.put("a", b"source").json();
    let generation: i64 = source["generation"].as_str().unwrap().parse().unwrap();
    let destination = server.put("destination", b"preserved").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER replace_compose_source AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE objects SET generation = generation + 1 WHERE name = 'a'; UPDATE object_generation SET last_generation = max(last_generation, (SELECT max(generation) FROM objects)); END;").unwrap();
    compose(
        &server,
        "destination",
        json!({"sourceObjects":[{"name":"a"}],"deleteSourceObjects":true}),
    )
    .error(404, "notFound");
    connection
        .execute_batch("DROP TRIGGER replace_compose_source;")
        .unwrap();
    assert_eq!(
        server.get("a").json()["generation"],
        (generation + 1).to_string()
    );
    assert_eq!(media(&server, "a").body, b"source");
    assert_eq!(server.get("destination").json(), destination);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn compose_destination_condition_is_rechecked_after_streaming() {
    let (dir, server) = setup(false);
    server.put("a", b"source").json();
    let destination = server.put("destination", b"preserved").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER change_compose_destination AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE objects SET metageneration = metageneration + 1 WHERE name = 'destination'; END;").unwrap();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/destination/compose?ifMetagenerationMatch=1",
            AUTH,
            sources(&["a"]).to_string().as_bytes(),
        )
        .error(412, "conditionNotMet");
    connection
        .execute_batch("DROP TRIGGER change_compose_destination;")
        .unwrap();
    let after = server.get("destination").json();
    assert_eq!(after["generation"], destination["generation"]);
    assert_eq!(after["metageneration"], "2");
    assert_eq!(media(&server, "destination").body, b"preserved");
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn compose_incarnation_change_preserves_original_error_and_recovers_after_restart() {
    let (dir, mut server) = setup(false);
    let source = server.put("a", b"source").json();
    let destination = server.put("destination", b"preserved").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("CREATE TRIGGER replace_compose_bucket AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE bucket_incarnations SET incarnation = CASE WHEN incarnation = '00000000000000000000000000000000' THEN '11111111111111111111111111111111' ELSE '00000000000000000000000000000000' END WHERE bucket = 'test-bucket'; END;
        CREATE TRIGGER fail_compose_cleanup BEFORE DELETE ON derived_transfers BEGIN SELECT RAISE(ABORT, 'injected compose cleanup failure'); END;").unwrap();
    compose(&server, "destination", sources(&["a"])).error(412, "conditionNotMet");
    assert_eq!(server.get("destination").json(), destination);
    assert_eq!(server.get("a").json(), source);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    connection
        .execute_batch("DROP TRIGGER replace_compose_bucket; DROP TRIGGER fail_compose_cleanup;")
        .unwrap();
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("destination").json(), destination);
    assert_eq!(server.get("a").json(), source);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn compose_rechecks_source_delete_grant_while_read_and_create_remain_allowed() {
    let (dir, server) = setup(true);
    let source = server.put("a", b"source").json();
    server.request("PUT", "/storage/v1/b/test-bucket/iam", AUTH, br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["user:creator@example.test"]},{"role":"roles/storage.objectUser","members":["user:creator@example.test"]}]}"#).json();
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
        .error(403, "forbidden");
    connection
        .execute_batch("DROP TRIGGER revoke_compose_delete;")
        .unwrap();
    server.get("destination").error(404, "notFound");
    assert_eq!(server.get("a").json(), source);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn compose_same_name_noncurrent_source_hold_is_rechecked_before_publication() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let old = server.put("destination", b"old").json();
    let generation: i64 = old["generation"].as_str().unwrap().parse().unwrap();
    let current = server.put("destination", b"current").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    // A held noncurrent self-source bypasses the ordinary live-destination hold check.
    connection.execute_batch(&format!("CREATE TRIGGER hold_compose_self_source AFTER UPDATE OF offset ON derived_transfers BEGIN UPDATE objects SET event_based_hold = 1 WHERE name = 'destination' AND generation = {generation}; END;")).unwrap();
    compose(&server,"destination",json!({"sourceObjects":[{"name":"destination","generation":generation.to_string()}],"deleteSourceObjects":true})).error(403,"objectUnderActiveHold");
    connection
        .execute_batch("DROP TRIGGER hold_compose_self_source;")
        .unwrap();
    assert_eq!(server.get("destination").json(), current);
    assert_eq!(media(&server, "destination").body, b"current");
    assert_eq!(
        server
            .get(&format!("destination?generation={generation}"))
            .json()["eventBasedHold"],
        true
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM derived_transfers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}
