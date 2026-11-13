use crate::common;
use common::TestDir;
use common::effects::{Server, request, settings};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Barrier},
    thread,
};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const BASE: &str = "/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/destination";

fn setup(strict: bool, size: usize) -> (TestDir, Server, Vec<u8>, Value) {
    let dir = TestDir::new();
    let server = Server::start(&dir, strict);
    server.bucket();
    let payload: Vec<_> = (0..size).map(|index| (index % 251) as u8).collect();
    let source = server.put("source", &payload).json_ok();
    (dir, server, payload, source)
}
fn multi(server: &Server, query: &str) -> Value {
    server
        .request(
            "POST",
            &format!("{BASE}?maxBytesRewrittenPerCall=1048576{query}"),
            ADMIN,
            br#"{"storageClass":"NEARLINE","metadata":{"team":"rewrite"}}"#,
        )
        .json_ok()
}
fn continue_path(token: &str) -> String {
    format!("{BASE}?rewriteToken={token}")
}

#[test]
fn rewrite_keeps_an_explicit_false_hold_and_ignores_null_custom_time() {
    let (_dir, server, payload, _) = setup(false, 19);
    for body in [
        br#"{"temporaryHold":false}"#.as_slice(),
        br#"{"customTime":null}"#,
    ] {
        let result = server.request("POST", BASE, ADMIN, body).json_ok();
        assert_eq!(result["done"], true);
        // An explicit false is kept as sent (not recorded at create time); null names nothing.
        assert_eq!(
            result["resource"]
                .get("temporaryHold")
                .and_then(Value::as_bool),
            std::str::from_utf8(body)
                .unwrap()
                .contains("false")
                .then_some(false)
        );
        assert!(result["resource"].get("customTime").is_none());
        assert_eq!(server.get("destination?alt=media").body, payload);
    }
    let before = server.get("destination").json_ok();
    server
        .request(
            "POST",
            BASE,
            ADMIN,
            br#"{"customTime":"2026-10-20T01:02:03Z"}"#,
        )
        .json_reason(400, "badRequest");
    assert_eq!(server.get("destination").json_ok(), before);
}

#[test]
fn rewrite_one_call_empty_metadata_fields_and_same_name() {
    for size in [0, 19] {
        let (_dir, server, payload, source) = setup(false, size);
        server
            .request(
                "PATCH",
                "/storage/v1/b/test-bucket/o/source",
                ADMIN,
                br#"{"contentType":"text/plain","metadata":{"original":"value"}}"#,
            )
            .json_ok();
        let result = server.request("POST", BASE, ADMIN, b"").json_ok();
        assert_eq!(result["kind"], "storage#rewriteResponse");
        assert_eq!(result["done"], true);
        assert_eq!(result["objectSize"], size.to_string());
        assert_eq!(result["totalBytesRewritten"], size.to_string());
        assert!(result.get("rewriteToken").is_none());
        assert_eq!(result["resource"]["contentType"], "text/plain");
        assert_eq!(result["resource"]["metadata"]["original"], "value");
        let null_body = server.request("POST", BASE, ADMIN, b" \nnull\t").json_ok();
        assert_eq!(null_body["resource"]["contentType"], "text/plain");
        assert_eq!(null_body["resource"]["metadata"]["original"], "value");
        for hash in ["md5Hash", "crc32c"] {
            assert_eq!(result["resource"][hash], source[hash]);
        }
        assert_eq!(server.get("destination?alt=media").body, payload);
        let same=server.request("POST","/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/source?fields=done,resource(generation,name)",ADMIN,b"{}").json_ok();
        assert_eq!(same["done"], true);
        assert_ne!(same["resource"]["generation"], source["generation"]);
        assert!(same.get("objectSize").is_none());
        assert_eq!(server.get("source?alt=media").body, payload);
    }
}

#[test]
fn abandoned_rewrites_past_the_former_quota_survive_a_restart_and_can_finish() {
    // One past the 128 unfinished rewrites that used to fail requests and refuse to reopen.
    const ABANDONED: usize = 129;
    let (dir, mut server, payload, _source) = setup(false, 1048576 + 7);
    let tokens: Vec<String> = (0..ABANDONED)
        .map(|_| {
            let first = multi(&server, "");
            assert_eq!(first["done"], false);
            first["rewriteToken"].as_str().unwrap().to_owned()
        })
        .collect();
    server.stop();
    let server = Server::start(&dir, false);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        common::catalog::count(&connection, "rewrite_sessions"),
        ABANDONED as i64
    );
    let completed = server
        .request("POST", &continue_path(&tokens[ABANDONED - 1]), ADMIN, b"")
        .json_ok();
    assert_eq!(completed["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn a_configured_rewrite_limit_preserves_copy_and_compose_capacity_after_restart() {
    let args = ["--max-rewrite-sessions", "128"];
    let dir = TestDir::new();
    let mut server = Server::start_with_args(&dir, &args);
    server.bucket();
    let payload: Vec<_> = (0..1048576 + 7).map(|index| (index % 251) as u8).collect();
    let source = server.put("source", &payload).json_ok();
    let mut token = None;
    for _ in 0..128 {
        let first = multi(&server, "");
        assert_eq!(first["done"], false);
        token.get_or_insert(first["rewriteToken"].as_str().unwrap().to_owned());
    }
    for restart in [false, true] {
        if restart {
            server.stop();
            server = Server::start_with_args(&dir, &args);
        }
        let refused = server.request(
            "POST",
            &format!("{BASE}?maxBytesRewrittenPerCall=1048576"),
            ADMIN,
            br#"{"storageClass":"NEARLINE"}"#,
        );
        assert!(
            String::from_utf8_lossy(&refused.body).contains("max_rewrite_sessions"),
            "{}",
            String::from_utf8_lossy(&refused.body)
        );
        refused.json_reason(503, "backendError");
        server.get("destination").json_reason(404, "notFound");
        let copied = server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/copied",
                ADMIN,
                b"",
            )
            .json_ok();
        assert_eq!(copied["md5Hash"], source["md5Hash"]);
        assert_eq!(server.get("copied?alt=media").body, payload);
        let composed = server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/composed/compose",
                ADMIN,
                br#"{"sourceObjects":[{"name":"source"},{"name":"copied"}]}"#,
            )
            .json_ok();
        assert_eq!(composed["componentCount"], 2);
        assert_eq!(server.get("composed?alt=media").body, payload.repeat(2));
        let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        for table in ["derived_transfers", "rewrite_sessions"] {
            assert_eq!(common::catalog::count(&connection, table), 128);
        }
        assert_eq!(
            common::catalog::count(&connection, "derived_transfers WHERE ephemeral = 1"),
            0
        );
        assert_eq!(common::process::staging_count(&dir), 256);
    }
    let completed = server
        .request("POST", &continue_path(&token.unwrap()), ADMIN, b"")
        .json_ok();
    assert_eq!(completed["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(multi(&server, "")["done"], false);
}

#[test]
fn a_rewrite_continuation_ignores_a_changed_or_missing_api_key() {
    let (_dir, server, payload, _source) = setup(false, 2 * 1048576 + 17);
    let first = multi(&server, "&key=first-key");
    assert_eq!(first["done"], false);
    let token = first["rewriteToken"].as_str().unwrap();
    let second = server
        .request(
            "POST",
            &format!("{}&key=second-key", continue_path(token)),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(second["done"], false);
    let token = second["rewriteToken"].as_str().unwrap();
    let completed = server
        .request("POST", &continue_path(token), ADMIN, b"")
        .json_ok();
    assert_eq!(completed["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn rewrite_progress_retry_restart_and_completion_replay_publish_once() {
    let (dir, mut server, payload, _source) = setup(false, 2 * 1048576 + 17);
    let first = multi(&server, "&ifGenerationMatch=0");
    assert_eq!(first["done"], false);
    assert_eq!(first["totalBytesRewritten"], "1048576");
    assert!(first.get("resource").is_none());
    server.get("destination").json_reason(404, "notFound");
    let first_token = first["rewriteToken"].as_str().unwrap();
    let path = continue_path(first_token);
    let second = server.request("POST", &path, ADMIN, b"").json_ok();
    assert_eq!(second["totalBytesRewritten"], "2097152");
    assert_eq!(server.request("POST", &path, ADMIN, b"").json_ok(), second);
    server.stop();
    let server = Server::start(&dir, false);
    let last_path = continue_path(second["rewriteToken"].as_str().unwrap());
    let completed = server.request("POST", &last_path, ADMIN, b"").json_ok();
    assert_eq!(completed["done"], true);
    assert_eq!(completed["totalBytesRewritten"], payload.len().to_string());
    assert!(completed.get("rewriteToken").is_none());
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(
        server.get("destination").json_ok(),
        common::without_acl(completed["resource"].clone())
    );
    server.put("destination", b"later").json_ok();
    assert_eq!(
        server.request("POST", &last_path, ADMIN, b"").json_ok(),
        completed
    );
    assert_eq!(server.get("destination?alt=media").body, b"later");
    assert_eq!(
        server.list("versions=true")["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn rewrite_tokens_cannot_change_path_parameters_body_or_byte_budget() {
    let (_dir, server, _payload, source) = setup(false, 1048576 + 7);
    let first = multi(
        &server,
        &format!(
            "&sourceGeneration={}&ifGenerationMatch=0&ifSourceMetagenerationMatch=1",
            source["generation"].as_str().unwrap()
        ),
    );
    let token = first["rewriteToken"].as_str().unwrap();
    let path = continue_path(token);
    for query in [
        "ifGenerationMatch=9",
        "maxBytesRewrittenPerCall=2097152",
        "sourceGeneration=9",
        "ifSourceMetagenerationMatch=9",
        "fields=done",
    ] {
        server
            .request("POST", &format!("{path}&{query}"), ADMIN, b"")
            .json_reason(400, "invalid");
    }
    // `{}` means no resource, like the empty body the session started with; a real resource
    // changes the body the token is bound to.
    server
        .request("POST", &path, ADMIN, br#"{"metadata":{"k":"v"}}"#)
        .json_reason(400, "invalid");
    server
        .request(
            "POST",
            &path.replace("/o/destination?", "/o/other?"),
            ADMIN,
            b"",
        )
        .json_reason(400, "invalid");
    for index in 0..token.len() {
        let mut tampered = token.to_owned();
        tampered.replace_range(
            index..index + 1,
            if token.as_bytes()[index] == b'0' {
                "1"
            } else {
                "0"
            },
        );
        server
            .request("POST", &continue_path(&tampered), ADMIN, b"")
            .json_reason(410, "gone");
    }
    server
        .request("POST", &continue_path("not-a-token"), ADMIN, b"")
        .json_reason(400, "invalid");
    assert_eq!(
        server.request("POST", &path, ADMIN, b"").json_ok()["done"],
        true
    );
}

#[test]
fn rewrite_source_overwrite_delete_and_metadata_change_fail_closed() {
    for mutation in ["overwrite", "delete", "metadata"] {
        let (dir, server, _payload, _source) = setup(false, 1048576 + 3);
        let first = multi(&server, "");
        match mutation {
            "overwrite" => {
                server.put("source", b"replacement").json_ok();
            }
            "delete" => {
                server
                    .request("DELETE", "/storage/v1/b/test-bucket/o/source", ADMIN, b"")
                    .expect(204);
            }
            _ => {
                server
                    .request(
                        "PATCH",
                        "/storage/v1/b/test-bucket/o/source",
                        ADMIN,
                        br#"{"metadata":{"changed":"yes"}}"#,
                    )
                    .json_ok();
            }
        }
        server
            .request(
                "POST",
                &continue_path(first["rewriteToken"].as_str().unwrap()),
                ADMIN,
                b"",
            )
            .json_reason(412, "conditionNotMet");
        server.get("destination").json_reason(404, "notFound");
        server
            .request(
                "POST",
                &continue_path(first["rewriteToken"].as_str().unwrap()),
                ADMIN,
                b"",
            )
            .json_reason(410, "gone");
        let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        for table in ["rewrite_sessions", "derived_transfers", "derived_inputs"] {
            assert_eq!(common::catalog::count(&connection, table), 0);
        }
        assert_eq!(common::process::staging_count(&dir), 0);
    }
}

#[test]
fn rewrite_all_conditions_preserve_failed_destination_and_retained_source() {
    let (dir, server, payload, source) = setup(false, 17);
    let old = server.put("destination", b"previous").json_ok();
    for key in [
        "ifGenerationMatch",
        "ifMetagenerationMatch",
        "ifSourceGenerationMatch",
        "ifSourceMetagenerationMatch",
    ] {
        server
            .request("POST", &format!("{BASE}?{key}=999"), ADMIN, b"")
            .json_reason(412, "conditionNotMet");
        assert_eq!(server.get("destination").json_ok(), old);
    }
    // A destination not-match naming the current destination is an empty 304, as recorded
    // for copy; source not-match conditions stay failed preconditions.
    for (key, value) in [
        ("ifGenerationNotMatch", old["generation"].as_str().unwrap()),
        ("ifMetagenerationNotMatch", "1"),
    ] {
        let reply = server.request("POST", &format!("{BASE}?{key}={value}"), ADMIN, b"");
        reply.expect(304);
        assert!(reply.body.is_empty(), "{key}");
        assert_eq!(server.get("destination").json_ok(), old);
    }
    for (key, value) in [
        (
            "ifSourceGenerationNotMatch",
            source["generation"].as_str().unwrap(),
        ),
        ("ifSourceMetagenerationNotMatch", "1"),
    ] {
        server
            .request("POST", &format!("{BASE}?{key}={value}"), ADMIN, b"")
            .json_reason(412, "conditionNotMet");
        assert_eq!(server.get("destination").json_ok(), old);
    }
    settings(&dir, true, false);
    server.put("source", b"new").json_ok();
    let copied = server
        .request(
            "POST",
            &format!(
                "{BASE}?sourceGeneration={}",
                source["generation"].as_str().unwrap()
            ),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(copied["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn rewrite_create_if_absent_race_has_one_atomic_winner() {
    let (_dir, server, payload, _source) = setup(false, 31);
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let address = server.address.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                request(
                    &address,
                    "POST",
                    &format!("{BASE}?ifGenerationMatch=0"),
                    ADMIN,
                    b"",
                )
                .status
            })
        })
        .collect();
    barrier.wait();
    let mut statuses: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    statuses.sort();
    assert_eq!(statuses, [200, 412]);
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn rewrite_strict_permissions_holds_versioning_and_unsupported_inputs() {
    let (dir, server, payload, _source) = setup(true, 41);
    for (header, status) in [
        ("", 401),
        ("Authorization: Bearer viewer\r\n", 403),
        ("Authorization: Bearer creator\r\n", 403),
    ] {
        assert_eq!(server.request("POST", BASE, header, b"").status, status);
    }
    settings(&dir, true, true);
    let first = server.request("POST", BASE, ADMIN, b"").json_ok();
    assert_eq!(first["resource"]["eventBasedHold"], true);
    server.request("POST", BASE, ADMIN, b"").hold_error_reason();
    assert_eq!(server.get("destination?alt=media").body, payload);
    for query in [
        "destinationKmsKeyName=key",
        "destinationPredefinedAcl=private",
        "quotaUser=project",
        "dropContextGroups=custom",
    ] {
        server
            .request("POST", &format!("{BASE}?{query}"), ADMIN, b"")
            .json_reason(400, "badRequest");
    }
    for field in ["acl", "retention", "contexts", "kmsKeyName"] {
        server
            .request(
                "POST",
                BASE,
                ADMIN,
                json!({field:{}}).to_string().as_bytes(),
            )
            .json_reason(400, "badRequest");
    }
    server
        .request("POST", BASE, ADMIN, br#"{"temporaryHold":true}"#)
        .json_reason(400, "badRequest");
    assert_eq!(
        first["resource"]["owner"],
        json!({"entity": "user-admin@example.test"})
    );
    assert_eq!(
        server.get("destination").json_ok(),
        common::without_acl(first["resource"].clone())
    );
}

#[test]
fn rewrite_expired_token_and_restart_cleanup_preserve_live_destination() {
    let (dir, mut server, _payload, _source) = setup(false, 1048576 + 3);
    let previous = server.put("destination", b"previous").json_ok();
    let first = multi(&server, "");
    let path = continue_path(first["rewriteToken"].as_str().unwrap());
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .execute("UPDATE rewrite_sessions SET deadline=0", [])
        .unwrap();
    server
        .request("POST", &path, ADMIN, b"")
        .json_reason(410, "gone");
    assert_eq!(server.get("destination").json_ok(), previous);
    server.stop();
    let server = Server::start(&dir, false);
    server
        .request("POST", &path, ADMIN, b"")
        .json_reason(410, "gone");
    assert_eq!(server.get("destination").json_ok(), previous);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    for table in ["rewrite_sessions", "derived_transfers", "derived_inputs"] {
        assert_eq!(common::catalog::count(&connection, table), 0);
    }
    assert_eq!(common::process::staging_count(&dir), 0);
}

#[test]
fn rewrite_continuation_rechecks_strict_grants_and_destination_conditions() {
    let (dir, server, _payload, _source) = setup(true, 1048576 + 9);
    let initial = multi(&server, "&ifGenerationMatch=0");
    let path = continue_path(initial["rewriteToken"].as_str().unwrap());
    server
        .request("POST", &path, "Authorization: Bearer viewer\r\n", b"")
        .json_reason(403, "forbidden");
    server
        .request("POST", &path, "Authorization: Bearer creator\r\n", b"")
        .json_reason(403, "forbidden");
    let concurrent = server.put("destination", b"concurrent").json_ok();
    server
        .request("POST", &path, ADMIN, b"")
        .json_reason(412, "conditionNotMet");
    assert_eq!(server.get("destination").json_ok(), concurrent);
    assert_eq!(server.get("destination?alt=media").body, b"concurrent");
    let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        common::catalog::count(&db, "objects WHERE name='destination'"),
        1
    );
}

#[test]
fn rewrite_concurrent_token_retry_has_one_progress_checkpoint() {
    let (_dir, server, _payload, _source) = setup(false, 3 * 1048576 + 7);
    let initial = multi(&server, "");
    let path = continue_path(initial["rewriteToken"].as_str().unwrap());
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let address = server.address.clone();
            let path = path.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                request(&address, "POST", &path, ADMIN, b"")
            })
        })
        .collect();
    barrier.wait();
    let replies: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let mut success = None;
    for reply in replies {
        if reply.status == 200 {
            let json = reply.json_ok();
            assert_eq!(json["totalBytesRewritten"], "2097152");
            if let Some(previous) = &success {
                assert_eq!(previous, &json);
            }
            success = Some(json);
        } else {
            reply.json_reason(503, "backendError");
        }
    }
    assert_eq!(
        server.request("POST", &path, ADMIN, b"").json_ok(),
        success.expect("one retry must succeed")
    );
}

#[test]
fn rewrite_cross_bucket_grants_are_rechecked_without_copying_source_holds() {
    let (_dir, server, payload, _source) = setup(true, 1048576 + 5);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"destination-bucket","location":"EU"}"#,
        )
        .json_ok();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/source",
            ADMIN,
            br#"{"eventBasedHold":true}"#,
        )
        .json_ok();
    let set_policy = |bindings: Value| {
        server
            .request(
                "PUT",
                "/storage/v1/b/destination-bucket/iam",
                ADMIN,
                json!({"version":1,"bindings":bindings})
                    .to_string()
                    .as_bytes(),
            )
            .json_ok()
    };
    let grant =
        json!([{"role":"roles/storage.objectCreator","members":["user:viewer@example.test"]}]);
    set_policy(grant.clone());
    let base = "/storage/v1/b/test-bucket/o/source/rewriteTo/b/destination-bucket/o/destination";
    let viewer = "Authorization: Bearer viewer\r\n";
    let first = server
        .request(
            "POST",
            &format!("{base}?maxBytesRewrittenPerCall=1048576&ifGenerationMatch=0"),
            viewer,
            b"",
        )
        .json_ok();
    assert_eq!(first["done"], false);
    let path = format!(
        "{base}?rewriteToken={}",
        first["rewriteToken"].as_str().unwrap()
    );
    set_policy(json!([]));
    server
        .request("POST", &path, viewer, b"")
        .json_reason(403, "forbidden");
    set_policy(grant);
    let complete = server.request("POST", &path, viewer, b"").json_ok();
    assert_eq!(complete["done"], true);
    assert_ne!(complete["resource"]["eventBasedHold"], true);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/destination-bucket/o/destination?alt=media",
                ADMIN,
                b""
            )
            .body,
        payload
    );
    server
        .request("POST", base, viewer, b"")
        .json_reason(403, "forbidden");
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/destination-bucket/o/destination",
                ADMIN,
                b""
            )
            .json_ok(),
        common::without_acl(complete["resource"].clone())
    );
}

#[test]
fn rewrite_explicit_retained_generation_can_continue_after_a_live_overwrite() {
    let (dir, server, payload, source) = setup(false, 1048576 + 1);
    settings(&dir, true, false);
    let first = multi(
        &server,
        &format!(
            "&sourceGeneration={}",
            source["generation"].as_str().unwrap()
        ),
    );
    server.put("source", b"new live revision").json_ok();
    let complete = server
        .request(
            "POST",
            &continue_path(first["rewriteToken"].as_str().unwrap()),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(complete["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(server.get("source?alt=media").body, b"new live revision");
}

#[test]
fn rewrite_process_kill_recovers_acknowledged_prefix_and_discards_uncommitted_tail() {
    use std::io::Write;
    let (dir, mut server, payload, _source) = setup(false, 1048576 + 17);
    let first = multi(&server, "");
    let path = continue_path(first["rewriteToken"].as_str().unwrap());
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    let id: String = rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .query_row("SELECT id FROM rewrite_sessions", [], |row| row.get(0))
        .unwrap();
    let stage = dir.0.join("staging").join(id);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&stage)
        .unwrap()
        .write_all(b"unacknowledged suffix")
        .unwrap();
    let server = Server::start(&dir, false);
    assert_eq!(std::fs::metadata(stage).unwrap().len(), 1048576);
    let complete = server.request("POST", &path, ADMIN, b"").json_ok();
    assert_eq!(complete["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(
        server.get("destination").json_ok(),
        common::without_acl(complete["resource"].clone())
    );
}

#[test]
fn rewrite_metadata_checksums_validate_and_identity_is_path_selected() {
    let (_dir, server, payload, source) = setup(false, 23);
    let previous = server.put("destination", b"previous").json_ok();
    server
        .request("POST", BASE, ADMIN, br#"{"crc32c":"AAAAAA=="}"#)
        .json_reason(400, "invalid");
    assert_eq!(server.get("destination").json_ok(), previous);
    // Cloud Storage ignores a supplied size and md5Hash on rewrite and completes with the
    // source's values (recorded 2026-11-13).
    let ignored = server
        .request(
            "POST",
            BASE,
            ADMIN,
            br#"{"size":"999","md5Hash":"AAAAAAAAAAAAAAAAAAAAAA=="}"#,
        )
        .json_ok();
    assert_eq!(ignored["resource"]["size"], source["size"]);
    assert_eq!(ignored["resource"]["md5Hash"], source["md5Hash"]);
    // A null or empty metadata map, like a blank body, inherits the source metadata.
    for body in [&br#"{"metadata":null}"#[..], br#"{"metadata":{}}"#, b" "] {
        let kept = server.request("POST", BASE, ADMIN, body).json_ok();
        assert_eq!(kept["resource"]["contentType"], source["contentType"]);
    }
    let body = json!({"kind":"storage#object","name":"ignored","bucket":"ignored","md5Hash":source["md5Hash"],"crc32c":source["crc32c"],"contentType":"application/x-rewrite","metadata":{"team":"雪"}});
    let completed = server
        .request("POST", BASE, ADMIN, body.to_string().as_bytes())
        .json_ok();
    assert_eq!(completed["resource"]["name"], "destination");
    assert_eq!(completed["resource"]["bucket"], "test-bucket");
    assert_eq!(completed["resource"]["metadata"]["team"], "雪");
    assert_eq!(server.get("destination?alt=media").body, payload);
    // An empty resource keeps the source metadata like an absent one (recorded for `{}`).
    let kept = server.request("POST", BASE, ADMIN, b"{}").json_ok();
    assert_eq!(kept["resource"]["contentType"], source["contentType"]);
    let cleared = server
        .request("POST", BASE, ADMIN, br#"{"cacheControl":"no-cache"}"#)
        .json_ok();
    assert!(cleared["resource"].get("metadata").is_none());
    assert!(cleared["resource"].get("contentType").is_none());
    assert_eq!(
        server.get("destination?alt=media").headers["content-type"],
        "application/octet-stream"
    );
}

#[test]
fn rewrite_invalid_acknowledged_prefix_is_cleanly_rejected_after_restart() {
    use std::io::Write;
    for damage in ["missing", "truncated", "corrupted"] {
        let (dir, mut server, _payload, _source) = setup(false, 1048576 + 1);
        let previous = server.put("destination", b"previous").json_ok();
        let first = multi(&server, "");
        let path = continue_path(first["rewriteToken"].as_str().unwrap());
        server.stop();
        let id: String = rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
            .unwrap()
            .query_row("SELECT id FROM rewrite_sessions", [], |row| row.get(0))
            .unwrap();
        let stage = dir.0.join("staging").join(id);
        match damage {
            "missing" => std::fs::remove_file(stage).unwrap(),
            "truncated" => std::fs::OpenOptions::new()
                .write(true)
                .open(stage)
                .unwrap()
                .set_len(0)
                .unwrap(),
            _ => std::fs::OpenOptions::new()
                .write(true)
                .open(stage)
                .unwrap()
                .write_all(&[255])
                .unwrap(),
        }
        let server = Server::start(&dir, false);
        server
            .request("POST", &path, ADMIN, b"")
            .json_reason(410, "gone");
        assert_eq!(server.get("destination").json_ok(), previous);
        assert_eq!(server.get("destination?alt=media").body, b"previous");
        assert_eq!(
            common::catalog::count(
                &rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap(),
                "rewrite_sessions"
            ),
            0
        );
    }
}

#[test]
fn rewrite_completion_replay_rejects_recreated_buckets() {
    let (_dir, server, _payload, _source) = setup(false, 1048576 + 1);
    let first = multi(&server, "");
    let path = continue_path(first["rewriteToken"].as_str().unwrap());
    assert_eq!(
        server.request("POST", &path, ADMIN, b"").json_ok()["done"],
        true
    );
    for name in ["source", "destination"] {
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/test-bucket/o/{name}"),
                ADMIN,
                b"",
            )
            .expect(204);
    }
    server
        .request("DELETE", "/storage/v1/b/test-bucket", ADMIN, b"")
        .expect(204);
    server.bucket();
    server.put("source", b"new bucket incarnation").json_ok();
    server
        .request("POST", &path, ADMIN, b"")
        .json_reason(412, "conditionNotMet");
    server.get("destination").json_reason(404, "notFound");
}

#[test]
fn rewrite_progress_replay_reclaims_state_after_bucket_recreation() {
    let (dir, server, _payload, _source) = setup(false, 3 * 1048576 + 1);
    let first = multi(&server, "");
    let replay_path = continue_path(first["rewriteToken"].as_str().unwrap());
    let second = server.request("POST", &replay_path, ADMIN, b"").json_ok();
    assert_eq!(second["done"], false);
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/source", ADMIN, b"")
        .expect(204);
    server
        .request("DELETE", "/storage/v1/b/test-bucket", ADMIN, b"")
        .expect(204);
    server.bucket();
    server
        .request("POST", &replay_path, ADMIN, b"")
        .json_reason(412, "conditionNotMet");
    for path in [
        replay_path,
        continue_path(second["rewriteToken"].as_str().unwrap()),
    ] {
        server
            .request("POST", &path, ADMIN, b"")
            .json_reason(410, "gone");
    }
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    for table in ["rewrite_sessions", "derived_transfers", "derived_inputs"] {
        assert_eq!(common::catalog::count(&connection, table), 0);
    }
    assert_eq!(common::process::staging_count(&dir), 0);
    server.get("destination").json_reason(404, "notFound");
}

#[test]
fn rewrite_composite_source_preserves_or_resets_component_count_without_md5() {
    let (dir, server, payload, source) = setup(false, 37);
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE objects SET component_count=3 WHERE name='source'",
            [],
        )
        .unwrap();
    let same = server.request("POST", BASE, ADMIN, b"").json_ok();
    assert_eq!(same["resource"]["componentCount"], 3);
    assert!(same["resource"].get("md5Hash").is_none());
    assert_eq!(same["resource"]["crc32c"], source["crc32c"]);
    let changed = server
        .request("POST", BASE, ADMIN, br#"{"storageClass":"NEARLINE"}"#)
        .json_ok();
    assert_eq!(changed["resource"]["componentCount"], 1);
    assert!(changed["resource"].get("md5Hash").is_none());
    assert_eq!(changed["resource"]["crc32c"], source["crc32c"]);
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn rewrite_composite_count_uses_the_bucket_class_selected_at_publication() {
    for (class, count) in [("NEARLINE", 3), ("COLDLINE", 1)] {
        let (dir, server, payload, source) = setup(false, 1048576 + 3);
        rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE objects SET component_count=3, storage_class='NEARLINE' WHERE name='source'",
                [],
            )
            .unwrap();
        let first = server
            .request(
                "POST",
                &format!("{BASE}?maxBytesRewrittenPerCall=1048576"),
                ADMIN,
                br#"{"metadata":{"k":"v"}}"#,
            )
            .json_ok();
        assert_eq!(first["done"], false);
        server
            .request(
                "PATCH",
                "/storage/v1/b/test-bucket",
                ADMIN,
                json!({"storageClass":class}).to_string().as_bytes(),
            )
            .json_ok();
        let complete = server
            .request(
                "POST",
                &continue_path(first["rewriteToken"].as_str().unwrap()),
                ADMIN,
                b"",
            )
            .json_ok();
        assert_eq!(complete["done"], true);
        assert_eq!(complete["resource"]["storageClass"], class);
        assert_eq!(complete["resource"]["componentCount"], count);
        assert!(complete["resource"].get("md5Hash").is_none());
        assert_eq!(complete["resource"]["crc32c"], source["crc32c"]);
        assert_eq!(server.get("destination?alt=media").body, payload);
    }
}

#[test]
fn rewrite_large_source_uses_request_budget_instead_of_catalog_wait_budget() {
    let (dir, mut server, payload, source) = setup(false, 16 * 1048576 + 17);
    server.stop();
    let server = Server::start_with_catalog_wait(&dir, false, 1);
    let started = std::time::Instant::now();
    let complete = server.request("POST", BASE, ADMIN, b"").json_ok();
    assert!(started.elapsed() > std::time::Duration::from_millis(1));
    assert_eq!(complete["done"], true);
    assert_eq!(complete["totalBytesRewritten"], payload.len().to_string());
    for hash in ["md5Hash", "crc32c"] {
        assert_eq!(complete["resource"][hash], source[hash]);
    }
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn rewrite_expired_completed_session_never_journals_the_live_blob_on_restart() {
    let (dir, mut server, payload, _source) = setup(false, 29);
    let complete = server.request("POST", BASE, ADMIN, b"").json_ok();
    server.stop();
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .execute("UPDATE rewrite_sessions SET deadline=0", [])
        .unwrap();
    let server = Server::start(&dir, false);
    assert_eq!(
        server.get("destination").json_ok(),
        common::without_acl(complete["resource"].clone())
    );
    assert_eq!(server.get("destination?alt=media").body, payload);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    for table in ["rewrite_sessions", "pending_blob_deletions"] {
        assert_eq!(common::catalog::count(&connection, table), 0);
    }
}

#[test]
fn rewrite_completed_cache_evicts_without_rejecting_new_rewrites() {
    let (dir, server, payload, _source) = setup(false, 1);
    server.request("POST", BASE, ADMIN, b"").json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let first: String = connection
        .query_row("SELECT token FROM rewrite_sessions", [], |row| row.get(0))
        .unwrap();
    // Copies of the finished row fill the window without thousands of requests. Their later
    // deadlines leave the original as the oldest record.
    connection
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<4095) INSERT INTO rewrite_sessions SELECT printf('%032x',i), printf('%064x',i), printf('%064x',i), r.request, r.offset, r.total, r.deadline+i, r.response, NULL FROM n, (SELECT * FROM rewrite_sessions LIMIT 1) r",
            [],
        )
        .unwrap();
    // A full window still answers its oldest record.
    server
        .request("POST", &continue_path(&first), ADMIN, b"")
        .json_ok();
    assert_eq!(
        server.request("POST", BASE, ADMIN, b"").json_ok()["done"],
        true
    );
    assert_eq!(
        common::catalog::count(&connection, "rewrite_sessions WHERE transfer_id IS NULL"),
        4096
    );
    assert_eq!(
        common::catalog::count(
            &connection,
            "rewrite_sessions WHERE transfer_id IS NOT NULL"
        ),
        0
    );
    server
        .request("POST", &continue_path(&first), ADMIN, b"")
        .json_reason(410, "gone");
    server
        .request("POST", &continue_path(&format!("{:064x}", 1)), ADMIN, b"")
        .json_ok();
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn rewrite_initial_catalog_wait_and_streaming_finish_before_returning_success() {
    let (dir, mut server, payload, _source) = setup(false, 16 * 1048576 + 17);
    server.stop();
    // The request budget must cover authorization and body parsing on a slow runner; the
    // catalog wait below outlasts it, so success shows the wait does not spend it.
    let request_budget = std::time::Duration::from_millis(300);
    let mut server = Server::start_with_timeouts(
        &dir,
        false,
        30000,
        u64::try_from(request_budget.as_millis()).unwrap(),
    );
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let address = server.address.clone();
    let started = std::time::Instant::now();
    let worker = thread::spawn(move || request(&address, "POST", BASE, ADMIN, b""));
    common::eventually("the rewrite staged its transfer", || {
        common::process::staging_count(&dir) >= 2
    });
    thread::sleep(
        (started + 2 * request_budget).saturating_duration_since(std::time::Instant::now()),
    );
    connection.execute_batch("COMMIT;").unwrap();
    let complete = worker.join().unwrap().json_ok();
    assert_eq!(complete["done"], true);
    assert_eq!(
        common::catalog::count(&connection, "objects WHERE name='destination'"),
        1
    );
    assert_eq!(
        common::catalog::count(
            &connection,
            "rewrite_sessions WHERE transfer_id IS NOT NULL"
        ),
        0
    );
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(
        server.get("destination").json_ok(),
        common::without_acl(complete["resource"].clone())
    );
}

#[test]
fn rewrite_revoked_overwrite_delete_permission_preserves_destination_and_releases_state() {
    let (dir, server, _payload, _source) = setup(true, 1048576 + 3);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"destination-bucket","location":"EU"}"#,
        )
        .json_ok();
    let old = server
        .request(
            "POST",
            "/upload/storage/v1/b/destination-bucket/o?uploadType=media&name=destination",
            ADMIN,
            b"previous",
        )
        .json_ok();
    let policy = |role: &str| {
        server.request("PUT","/storage/v1/b/destination-bucket/iam",ADMIN,json!({"version":1,"bindings":[{"role":role,"members":["user:viewer@example.test"]}]}).to_string().as_bytes()).json_ok()
    };
    policy("roles/storage.objectAdmin");
    let base = "/storage/v1/b/test-bucket/o/source/rewriteTo/b/destination-bucket/o/destination";
    let viewer = "Authorization: Bearer viewer\r\n";
    let first = server
        .request(
            "POST",
            &format!("{base}?maxBytesRewrittenPerCall=1048576"),
            viewer,
            b"",
        )
        .json_ok();
    assert_eq!(first["done"], false);
    policy("roles/storage.objectCreator");
    let path = format!(
        "{base}?rewriteToken={}",
        first["rewriteToken"].as_str().unwrap()
    );
    server
        .request("POST", &path, viewer, b"")
        .json_reason(403, "forbidden");
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/destination-bucket/o/destination",
                ADMIN,
                b""
            )
            .json_ok(),
        old
    );
    server
        .request("POST", &path, viewer, b"")
        .json_reason(410, "gone");
    assert_eq!(
        common::catalog::count(
            &rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap(),
            "rewrite_sessions"
        ),
        0
    );
}
