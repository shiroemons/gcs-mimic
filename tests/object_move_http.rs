mod object_effects_support;
use object_effects_support::{Reply, Server, common::TestDir, settings};
use rusqlite::Connection;
use serde_json::json;

const ADMIN: &str = "Authorization: Bearer admin\r\n";

fn setup(strict: bool) -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, strict);
    server.bucket();
    (dir, server)
}
fn path(source: &str, destination: &str, query: &str) -> String {
    format!("/storage/v1/b/test-bucket/o/{source}/moveTo/o/{destination}{query}")
}
fn mv(server: &Server, source: &str, destination: &str, query: &str) -> Reply {
    server.request("POST", &path(source, destination, query), ADMIN, b"")
}

#[test]
fn move_preserves_bytes_metadata_blob_and_restart() {
    let (dir, mut server) = setup(false);
    server.put("source%2F%E7%8C%AB%252F", b"move bytes").json();
    let original = server.request("PATCH", "/storage/v1/b/test-bucket/o/source%2F%E7%8C%AB%252F", ADMIN,
        br#"{"contentType":"text/plain","contentEncoding":"identity","contentDisposition":"inline","contentLanguage":"ja","cacheControl":"private","metadata":{"key":"value"},"customTime":"2026-10-20T01:02:03Z"}"#).json();
    let db = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let blob: String = db
        .query_row("SELECT blob_id FROM objects", [], |r| r.get(0))
        .unwrap();
    let moved = mv(
        &server,
        "source%2F%E7%8C%AB%252F",
        "target%2F%E7%8C%AB%252F",
        "",
    )
    .json();
    for key in [
        "contentType",
        "contentEncoding",
        "contentDisposition",
        "contentLanguage",
        "cacheControl",
        "metadata",
        "customTime",
        "storageClass",
        "size",
        "md5Hash",
        "crc32c",
    ] {
        assert_eq!(moved[key], original[key], "{key}");
    }
    assert_eq!(moved["name"], "target/猫%2F");
    assert_ne!(moved["generation"], original["generation"]);
    assert_ne!(moved["timeCreated"], original["timeCreated"]);
    assert_eq!(moved["metageneration"], "1");
    assert_eq!(
        db.query_row("SELECT blob_id FROM objects", [], |r| r.get::<_, String>(0))
            .unwrap(),
        blob
    );
    server.get("source%2F%E7%8C%AB%252F").error(404, "notFound");
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/target%2F%E7%8C%AB%252F?alt=media",
                ADMIN,
                b""
            )
            .body,
        b"move bytes"
    );
}

#[test]
fn move_checks_all_conditions_before_mutation() {
    let (_dir, server) = setup(false);
    let source = server.put("source", b"source").json();
    let target = server.put("target", b"target").json();
    for (key, value) in [
        ("ifSourceGenerationMatch", "0".to_owned()),
        (
            "ifSourceGenerationNotMatch",
            source["generation"].as_str().unwrap().to_owned(),
        ),
        ("ifSourceMetagenerationMatch", "2".to_owned()),
        ("ifSourceMetagenerationNotMatch", "1".to_owned()),
        ("ifGenerationMatch", "0".to_owned()),
        (
            "ifGenerationNotMatch",
            target["generation"].as_str().unwrap().to_owned(),
        ),
        ("ifMetagenerationMatch", "2".to_owned()),
        ("ifMetagenerationNotMatch", "1".to_owned()),
    ] {
        mv(&server, "source", "target", &format!("?{key}={value}")).error(412, "conditionNotMet");
        assert_eq!(server.get("source").json(), source);
        assert_eq!(server.get("target").json(), target);
    }
    for query in [
        "?ifGenerationMatch=0&ifGenerationNotMatch=1",
        "?ifSourceGenerationMatch=1&ifSourceGenerationNotMatch=2",
        "?ifMetagenerationMatch=1&ifMetagenerationNotMatch=2",
        "?ifSourceMetagenerationMatch=1&ifSourceMetagenerationNotMatch=2",
    ] {
        mv(&server, "source", "target", query).error(400, "invalidArgument");
    }
    mv(&server, "source", "target", &format!("?ifSourceGenerationMatch={}&ifSourceMetagenerationMatch=1&ifGenerationMatch={}&ifMetagenerationMatch=1", source["generation"].as_str().unwrap(), target["generation"].as_str().unwrap())).json();
    mv(
        &server,
        "target",
        "fresh",
        "?ifGenerationMatch=0&ifSourceGenerationNotMatch=0&ifSourceMetagenerationNotMatch=2",
    )
    .json();
}

#[test]
fn move_rejects_missing_same_name_holds_and_unsupported_inputs() {
    let (_dir, server) = setup(false);
    mv(&server, "missing", "target", "").error(404, "notFound");
    server.put("source", b"source").json();
    server.put("target", b"target").json();
    mv(&server, "source", "source", "").error(400, "invalidArgument");
    for field in ["temporaryHold", "eventBasedHold"] {
        for name in ["source", "target"] {
            server
                .request(
                    "PATCH",
                    &format!("/storage/v1/b/test-bucket/o/{name}"),
                    ADMIN,
                    json!({field:true}).to_string().as_bytes(),
                )
                .json();
            mv(&server, "source", "target", "").error(403, "objectUnderActiveHold");
            server
                .request(
                    "PATCH",
                    &format!("/storage/v1/b/test-bucket/o/{name}"),
                    ADMIN,
                    json!({field:false}).to_string().as_bytes(),
                )
                .json();
        }
    }
    for query in ["?userProject=project", "?projection=full"] {
        mv(&server, "source", "target", query).error(400, "badRequest");
    }
    server
        .request("POST", &path("source", "target", ""), ADMIN, b"{}")
        .error(400, "invalidArgument");
    assert_eq!(server.get("source").json()["name"], "source");
}

#[test]
fn move_versioning_deletes_live_source_and_archives_replaced_destination() {
    let (dir, server) = setup(false);
    settings(&dir, true, false);
    let old_source = server.put("source", b"old source").json();
    let source = server.put("source", b"source").json();
    let target = server.put("target", b"target").json();
    mv(&server, "source", "target", "").json();
    for (name, generation, status) in [
        ("source", &source["generation"], 404),
        ("source", &old_source["generation"], 200),
        ("target", &target["generation"], 200),
    ] {
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!(
                        "/storage/v1/b/test-bucket/o/{name}?generation={}",
                        generation.as_str().unwrap()
                    ),
                    ADMIN,
                    b""
                )
                .status,
            status
        );
    }
}

#[test]
fn move_rolls_back_source_deletion_when_destination_insert_fails() {
    let (dir, server) = setup(false);
    let source = server.put("source", b"source").json();
    let target = server.put("target", b"target").json();
    let db = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_move BEFORE INSERT ON objects WHEN NEW.name = 'target' BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END;").unwrap();
    assert_eq!(mv(&server, "source", "target", "").status, 503);
    assert_eq!(server.get("source").json(), source);
    assert_eq!(server.get("target").json(), target);
}

#[test]
fn strict_move_requires_source_read_delete_and_destination_create() {
    let (_dir, server) = setup(true);
    server.put("source", b"source").json();
    for token in ["viewer", "creator"] {
        server
            .request(
                "POST",
                &path("source", "target", ""),
                &format!("Authorization: Bearer {token}\r\n"),
                b"",
            )
            .error(403, "forbidden");
    }
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .json();
    server
        .request(
            "POST",
            &path("source", "target", ""),
            "Authorization: Bearer creator\r\n",
            b"",
        )
        .error(403, "forbidden");
    mv(&server, "source", "target", "").json();
}

#[test]
fn concurrent_moves_create_if_absent_have_one_winner() {
    let (_dir, server) = setup(false);
    server.put("first", b"first").json();
    server.put("second", b"second").json();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let replies = std::thread::scope(|scope| {
        let handles = ["first", "second"].map(|name| {
            let barrier = barrier.clone();
            let server = &server;
            scope.spawn(move || {
                barrier.wait();
                mv(server, name, "target", "?ifGenerationMatch=0")
            })
        });
        handles.map(|handle| handle.join().unwrap())
    });
    let mut statuses = replies.each_ref().map(|reply| reply.status);
    statuses.sort();
    assert_eq!(statuses, [200, 412]);
    let source_statuses = [server.get("first").status, server.get("second").status];
    assert_eq!(
        source_statuses
            .iter()
            .filter(|status| **status == 200)
            .count(),
        1
    );
    assert_eq!(
        source_statuses
            .iter()
            .filter(|status| **status == 404)
            .count(),
        1
    );
    assert_eq!(server.get("target").status, 200);
}
