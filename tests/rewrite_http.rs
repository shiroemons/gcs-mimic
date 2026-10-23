mod object_effects_support;
use object_effects_support::{Server, common::TestDir, request, settings};
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
    let source = server.put("source", &payload).json();
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
        .json()
}
fn continue_path(token: &str) -> String {
    format!("{BASE}?rewriteToken={token}")
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
            .json();
        let result = server.request("POST", BASE, ADMIN, b"").json();
        assert_eq!(result["kind"], "storage#rewriteResponse");
        assert_eq!(result["done"], true);
        assert_eq!(result["objectSize"], size.to_string());
        assert_eq!(result["totalBytesRewritten"], size.to_string());
        assert!(result.get("rewriteToken").is_none());
        assert_eq!(result["resource"]["contentType"], "text/plain");
        assert_eq!(result["resource"]["metadata"]["original"], "value");
        let null_body = server.request("POST", BASE, ADMIN, b" \nnull\t").json();
        assert_eq!(null_body["resource"]["contentType"], "text/plain");
        assert_eq!(null_body["resource"]["metadata"]["original"], "value");
        for hash in ["md5Hash", "crc32c"] {
            assert_eq!(result["resource"][hash], source[hash]);
        }
        assert_eq!(server.get("destination?alt=media").body, payload);
        let same=server.request("POST","/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/source?fields=done,resource(generation,name)",ADMIN,b"{}").json();
        assert_eq!(same["done"], true);
        assert_ne!(same["resource"]["generation"], source["generation"]);
        assert!(same.get("objectSize").is_none());
        assert_eq!(server.get("source?alt=media").body, payload);
    }
}

#[test]
fn abandoned_rewrite_quota_preserves_copy_and_compose_capacity_after_restart() {
    let (dir, mut server, payload, source) = setup(false, 1048576 + 7);
    let mut token = None;
    for _ in 0..128 {
        let first = multi(&server, "");
        assert_eq!(first["done"], false);
        token.get_or_insert(first["rewriteToken"].as_str().unwrap().to_owned());
    }
    for restart in [false, true] {
        if restart {
            server.stop();
            server = Server::start(&dir, false);
        }
        server
            .request(
                "POST",
                &format!("{BASE}?maxBytesRewrittenPerCall=1048576"),
                ADMIN,
                br#"{"storageClass":"NEARLINE"}"#,
            )
            .error(503, "backendError");
        server.get("destination").error(404, "notFound");
        let copied = server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/copied",
                ADMIN,
                b"",
            )
            .json();
        assert_eq!(copied["md5Hash"], source["md5Hash"]);
        assert_eq!(server.get("copied?alt=media").body, payload);
        let composed = server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/composed/compose",
                ADMIN,
                br#"{"sourceObjects":[{"name":"source"},{"name":"copied"}]}"#,
            )
            .json();
        assert_eq!(composed["componentCount"], 2);
        assert_eq!(server.get("composed?alt=media").body, payload.repeat(2));
        let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        for table in ["derived_transfers", "rewrite_sessions"] {
            assert_eq!(
                connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                128
            );
        }
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM derived_transfers WHERE ephemeral = 1",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            std::fs::read_dir(dir.0.join("staging")).unwrap().count(),
            256
        );
    }
    let completed = server
        .request("POST", &continue_path(&token.unwrap()), ADMIN, b"")
        .json();
    assert_eq!(completed["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(multi(&server, "")["done"], false);
}

#[test]
fn rewrite_progress_retry_restart_and_completion_replay_publish_once() {
    let (dir, mut server, payload, _source) = setup(false, 2 * 1048576 + 17);
    let first = multi(&server, "&ifGenerationMatch=0");
    assert_eq!(first["done"], false);
    assert_eq!(first["totalBytesRewritten"], "1048576");
    assert!(first.get("resource").is_none());
    server.get("destination").error(404, "notFound");
    let first_token = first["rewriteToken"].as_str().unwrap();
    let path = continue_path(first_token);
    let second = server.request("POST", &path, ADMIN, b"").json();
    assert_eq!(second["totalBytesRewritten"], "2097152");
    assert_eq!(server.request("POST", &path, ADMIN, b"").json(), second);
    server.stop();
    let server = Server::start(&dir, false);
    let last_path = continue_path(second["rewriteToken"].as_str().unwrap());
    let completed = server.request("POST", &last_path, ADMIN, b"").json();
    assert_eq!(completed["done"], true);
    assert_eq!(completed["totalBytesRewritten"], payload.len().to_string());
    assert!(completed.get("rewriteToken").is_none());
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(server.get("destination").json(), completed["resource"]);
    server.put("destination", b"later").json();
    assert_eq!(
        server.request("POST", &last_path, ADMIN, b"").json(),
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
            .error(400, "invalidParameter");
    }
    server
        .request("POST", &path, ADMIN, b"{}")
        .error(400, "invalidParameter");
    server
        .request(
            "POST",
            &path.replace("/o/destination?", "/o/other?"),
            ADMIN,
            b"",
        )
        .error(400, "invalidParameter");
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
            .error(410, "gone");
    }
    server
        .request("POST", &continue_path("not-a-token"), ADMIN, b"")
        .error(400, "invalidParameter");
    assert_eq!(
        server.request("POST", &path, ADMIN, b"").json()["done"],
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
                server.put("source", b"replacement").json();
            }
            "delete" => {
                assert_eq!(
                    server
                        .request("DELETE", "/storage/v1/b/test-bucket/o/source", ADMIN, b"")
                        .status,
                    204
                );
            }
            _ => {
                server
                    .request(
                        "PATCH",
                        "/storage/v1/b/test-bucket/o/source",
                        ADMIN,
                        br#"{"metadata":{"changed":"yes"}}"#,
                    )
                    .json();
            }
        }
        server
            .request(
                "POST",
                &continue_path(first["rewriteToken"].as_str().unwrap()),
                ADMIN,
                b"",
            )
            .error(412, "conditionNotMet");
        server.get("destination").error(404, "notFound");
        server
            .request(
                "POST",
                &continue_path(first["rewriteToken"].as_str().unwrap()),
                ADMIN,
                b"",
            )
            .error(410, "gone");
        let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        for table in ["rewrite_sessions", "derived_transfers", "derived_inputs"] {
            assert_eq!(
                connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    }
}

#[test]
fn rewrite_all_conditions_preserve_failed_destination_and_retained_source() {
    let (dir, server, payload, source) = setup(false, 17);
    let old = server.put("destination", b"previous").json();
    for key in [
        "ifGenerationMatch",
        "ifMetagenerationMatch",
        "ifSourceGenerationMatch",
        "ifSourceMetagenerationMatch",
    ] {
        server
            .request("POST", &format!("{BASE}?{key}=999"), ADMIN, b"")
            .error(412, "conditionNotMet");
        assert_eq!(server.get("destination").json(), old);
    }
    for (key, value) in [
        ("ifGenerationNotMatch", old["generation"].as_str().unwrap()),
        ("ifMetagenerationNotMatch", "1"),
        (
            "ifSourceGenerationNotMatch",
            source["generation"].as_str().unwrap(),
        ),
        ("ifSourceMetagenerationNotMatch", "1"),
    ] {
        server
            .request("POST", &format!("{BASE}?{key}={value}"), ADMIN, b"")
            .error(412, "conditionNotMet");
        assert_eq!(server.get("destination").json(), old);
    }
    settings(&dir, true, false);
    server.put("source", b"new").json();
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
        .json();
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
    let first = server.request("POST", BASE, ADMIN, b"").json();
    assert_eq!(first["resource"]["eventBasedHold"], true);
    server
        .request("POST", BASE, ADMIN, b"")
        .error(403, "objectUnderActiveHold");
    assert_eq!(server.get("destination?alt=media").body, payload);
    for query in [
        "destinationKmsKeyName=key",
        "destinationPredefinedAcl=private",
        "userProject=project",
        "dropContextGroups=custom",
    ] {
        server
            .request("POST", &format!("{BASE}?{query}"), ADMIN, b"")
            .error(501, "notImplemented");
    }
    for field in [
        "acl",
        "retention",
        "contexts",
        "temporaryHold",
        "kmsKeyName",
    ] {
        server
            .request(
                "POST",
                BASE,
                ADMIN,
                json!({field:{}}).to_string().as_bytes(),
            )
            .error(501, "notImplemented");
    }
    assert_eq!(server.get("destination").json(), first["resource"]);
}

#[test]
fn rewrite_expired_token_and_restart_cleanup_preserve_live_destination() {
    let (dir, mut server, _payload, _source) = setup(false, 1048576 + 3);
    let previous = server.put("destination", b"previous").json();
    let first = multi(&server, "");
    let path = continue_path(first["rewriteToken"].as_str().unwrap());
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .execute("UPDATE rewrite_sessions SET deadline=0", [])
        .unwrap();
    server.request("POST", &path, ADMIN, b"").error(410, "gone");
    assert_eq!(server.get("destination").json(), previous);
    server.stop();
    let server = Server::start(&dir, false);
    server.request("POST", &path, ADMIN, b"").error(410, "gone");
    assert_eq!(server.get("destination").json(), previous);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    for table in ["rewrite_sessions", "derived_transfers", "derived_inputs"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn rewrite_continuation_rechecks_strict_grants_and_destination_conditions() {
    let (dir, server, _payload, _source) = setup(true, 1048576 + 9);
    let initial = multi(&server, "&ifGenerationMatch=0");
    let path = continue_path(initial["rewriteToken"].as_str().unwrap());
    server
        .request("POST", &path, "Authorization: Bearer viewer\r\n", b"")
        .error(403, "forbidden");
    server
        .request("POST", &path, "Authorization: Bearer creator\r\n", b"")
        .error(403, "forbidden");
    let concurrent = server.put("destination", b"concurrent").json();
    server
        .request("POST", &path, ADMIN, b"")
        .error(412, "conditionNotMet");
    assert_eq!(server.get("destination").json(), concurrent);
    assert_eq!(server.get("destination?alt=media").body, b"concurrent");
    let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM objects WHERE name='destination'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
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
            let json = reply.json();
            assert_eq!(json["totalBytesRewritten"], "2097152");
            if let Some(previous) = &success {
                assert_eq!(previous, &json);
            }
            success = Some(json);
        } else {
            reply.error(503, "backendError");
        }
    }
    assert_eq!(
        server.request("POST", &path, ADMIN, b"").json(),
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
        .json();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/source",
            ADMIN,
            br#"{"eventBasedHold":true}"#,
        )
        .json();
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
            .json()
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
        .json();
    assert_eq!(first["done"], false);
    let path = format!(
        "{base}?rewriteToken={}",
        first["rewriteToken"].as_str().unwrap()
    );
    set_policy(json!([]));
    server
        .request("POST", &path, viewer, b"")
        .error(403, "forbidden");
    set_policy(grant);
    let complete = server.request("POST", &path, viewer, b"").json();
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
        .error(403, "forbidden");
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/destination-bucket/o/destination",
                ADMIN,
                b""
            )
            .json(),
        complete["resource"]
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
    server.put("source", b"new live revision").json();
    let complete = server
        .request(
            "POST",
            &continue_path(first["rewriteToken"].as_str().unwrap()),
            ADMIN,
            b"",
        )
        .json();
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
    let complete = server.request("POST", &path, ADMIN, b"").json();
    assert_eq!(complete["done"], true);
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(server.get("destination").json(), complete["resource"]);
}

#[test]
fn rewrite_metadata_checksums_validate_and_identity_is_path_selected() {
    let (_dir, server, payload, source) = setup(false, 23);
    let previous = server.put("destination", b"previous").json();
    for body in [
        json!({"md5Hash":"AAAAAAAAAAAAAAAAAAAAAA=="}),
        json!({"crc32c":"AAAAAA=="}),
    ] {
        server
            .request("POST", BASE, ADMIN, body.to_string().as_bytes())
            .error(400, "invalid");
        assert_eq!(server.get("destination").json(), previous);
    }
    let body = json!({"kind":"storage#object","name":"ignored","bucket":"ignored","md5Hash":source["md5Hash"],"crc32c":source["crc32c"],"contentType":"application/x-rewrite","metadata":{"team":"雪"}});
    let completed = server
        .request("POST", BASE, ADMIN, body.to_string().as_bytes())
        .json();
    assert_eq!(completed["resource"]["name"], "destination");
    assert_eq!(completed["resource"]["bucket"], "test-bucket");
    assert_eq!(completed["resource"]["metadata"]["team"], "雪");
    assert_eq!(server.get("destination?alt=media").body, payload);
    let cleared = server.request("POST", BASE, ADMIN, b"{}").json();
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
        let previous = server.put("destination", b"previous").json();
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
        server.request("POST", &path, ADMIN, b"").error(410, "gone");
        assert_eq!(server.get("destination").json(), previous);
        assert_eq!(server.get("destination?alt=media").body, b"previous");
        assert_eq!(
            rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
                .unwrap()
                .query_row("SELECT count(*) FROM rewrite_sessions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
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
        server.request("POST", &path, ADMIN, b"").json()["done"],
        true
    );
    for name in ["source", "destination"] {
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("/storage/v1/b/test-bucket/o/{name}"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
    }
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket", ADMIN, b"")
            .status,
        204
    );
    server.bucket();
    server.put("source", b"new bucket incarnation").json();
    server
        .request("POST", &path, ADMIN, b"")
        .error(412, "conditionNotMet");
    server.get("destination").error(404, "notFound");
}

#[test]
fn rewrite_progress_replay_reclaims_state_after_bucket_recreation() {
    let (dir, server, _payload, _source) = setup(false, 3 * 1048576 + 1);
    let first = multi(&server, "");
    let replay_path = continue_path(first["rewriteToken"].as_str().unwrap());
    let second = server.request("POST", &replay_path, ADMIN, b"").json();
    assert_eq!(second["done"], false);
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/source", ADMIN, b"")
            .status,
        204
    );
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket", ADMIN, b"")
            .status,
        204
    );
    server.bucket();
    server
        .request("POST", &replay_path, ADMIN, b"")
        .error(412, "conditionNotMet");
    for path in [
        replay_path,
        continue_path(second["rewriteToken"].as_str().unwrap()),
    ] {
        server.request("POST", &path, ADMIN, b"").error(410, "gone");
    }
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    for table in ["rewrite_sessions", "derived_transfers", "derived_inputs"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    server.get("destination").error(404, "notFound");
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
    let same = server.request("POST", BASE, ADMIN, b"").json();
    assert_eq!(same["resource"]["componentCount"], 3);
    assert!(same["resource"].get("md5Hash").is_none());
    assert_eq!(same["resource"]["crc32c"], source["crc32c"]);
    let changed = server
        .request("POST", BASE, ADMIN, br#"{"storageClass":"NEARLINE"}"#)
        .json();
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
                b"{}",
            )
            .json();
        assert_eq!(first["done"], false);
        server
            .request(
                "PATCH",
                "/storage/v1/b/test-bucket",
                ADMIN,
                json!({"storageClass":class}).to_string().as_bytes(),
            )
            .json();
        let complete = server
            .request(
                "POST",
                &continue_path(first["rewriteToken"].as_str().unwrap()),
                ADMIN,
                b"",
            )
            .json();
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
    let complete = server.request("POST", BASE, ADMIN, b"").json();
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
    let complete = server.request("POST", BASE, ADMIN, b"").json();
    server.stop();
    rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
        .unwrap()
        .execute("UPDATE rewrite_sessions SET deadline=0", [])
        .unwrap();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("destination").json(), complete["resource"]);
    assert_eq!(server.get("destination?alt=media").body, payload);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    for table in ["rewrite_sessions", "pending_blob_deletions"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn rewrite_completed_cache_evicts_without_rejecting_new_rewrites() {
    let (dir, server, payload, _source) = setup(false, 1);
    server.request("POST", BASE, ADMIN, b"").json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let first: String = connection
        .query_row("SELECT token FROM rewrite_sessions", [], |row| row.get(0))
        .unwrap();
    for _ in 0..129 {
        assert_eq!(
            server.request("POST", BASE, ADMIN, b"").json()["done"],
            true
        );
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        128
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    server
        .request("POST", &continue_path(&first), ADMIN, b"")
        .error(410, "gone");
    assert_eq!(server.get("destination?alt=media").body, payload);
}

#[test]
fn rewrite_initial_catalog_wait_and_streaming_finish_before_returning_success() {
    let (dir, mut server, payload, _source) = setup(false, 16 * 1048576 + 17);
    server.stop();
    let mut server = Server::start_with_timeouts(&dir, false, 30000, 1);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let address = server.address.clone();
    let worker = thread::spawn(move || request(&address, "POST", BASE, ADMIN, b""));
    let limit = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() < 2 {
        assert!(std::time::Instant::now() < limit);
        thread::sleep(std::time::Duration::from_millis(1));
    }
    thread::sleep(std::time::Duration::from_millis(20));
    connection.execute_batch("COMMIT;").unwrap();
    let complete = worker.join().unwrap().json();
    assert_eq!(complete["done"], true);
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM objects WHERE name='destination'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(server.get("destination?alt=media").body, payload);
    assert_eq!(server.get("destination").json(), complete["resource"]);
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
        .json();
    let old = server
        .request(
            "POST",
            "/upload/storage/v1/b/destination-bucket/o?uploadType=media&name=destination",
            ADMIN,
            b"previous",
        )
        .json();
    let policy = |role: &str| {
        server.request("PUT","/storage/v1/b/destination-bucket/iam",ADMIN,json!({"version":1,"bindings":[{"role":role,"members":["user:viewer@example.test"]}]}).to_string().as_bytes()).json()
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
        .json();
    assert_eq!(first["done"], false);
    policy("roles/storage.objectCreator");
    let path = format!(
        "{base}?rewriteToken={}",
        first["rewriteToken"].as_str().unwrap()
    );
    server
        .request("POST", &path, viewer, b"")
        .error(403, "forbidden");
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/destination-bucket/o/destination",
                ADMIN,
                b""
            )
            .json(),
        old
    );
    server
        .request("POST", &path, viewer, b"")
        .error(410, "gone");
    assert_eq!(
        rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
            .unwrap()
            .query_row("SELECT count(*) FROM rewrite_sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
