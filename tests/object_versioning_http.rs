mod object_effects_support;
use gcs_mimic::storage::admin::{BucketName, ObjectName, Origin, Publication, PublishMode};
use object_effects_support::{Server, blobs, common::TestDir, request, settings};
use std::{
    io::Write,
    net::TcpStream,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

fn setup(strict: bool) -> (TestDir, Server) {
    let dir = TestDir::new();
    let server = Server::start(&dir, strict);
    server.bucket();
    settings(&dir, true, false);
    (dir, server)
}

#[test]
fn versioned_overwrite_and_generation_reads() {
    let (dir, server) = setup(false);
    let first = server.put("a%2F%E9%9B%AA", b"first").json();
    let second = server.put("a%2F%E9%9B%AA", b"second").json();
    assert_ne!(first["generation"], second["generation"]);
    assert!(second.get("timeDeleted").is_none());
    let generation = first["generation"].as_str().unwrap();
    let old = server
        .get(&format!("a%2F%E9%9B%AA?generation={generation}"))
        .json();
    assert!(old["timeDeleted"].as_str().unwrap().ends_with('Z'));
    for field in [
        "generation",
        "metageneration",
        "md5Hash",
        "crc32c",
        "timeCreated",
        "updated",
    ] {
        assert_eq!(old[field], first[field], "{field}");
    }
    assert_eq!(
        server
            .get(&format!("a%2F%E9%9B%AA?generation={generation}&alt=media"))
            .body,
        b"first"
    );
    assert_eq!(
        server
            .get(&format!("a%2F%E9%9B%AA?generation={generation}&alt=media"))
            .status,
        200
    );
    assert_eq!(
        server
            .request(
                "GET",
                &format!("/test-bucket/a/%E9%9B%AA?generation={generation}"),
                "",
                b""
            )
            .body,
        b"first"
    );
    assert_eq!(server.get("a%2F%E9%9B%AA?alt=media").body, b"second");
    assert_eq!(server.list("")["items"].as_array().unwrap().len(), 1);
    assert_eq!(blobs(&dir), 2);
}

#[test]
fn versioned_delete_and_permanent_generation_delete() {
    let (dir, server) = setup(false);
    let first = server.put("file", b"first").json();
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket/o/file",
            "",
            br#"{"metadata":{"revision":"old"}}"#,
        )
        .json();
    let second = server.put("file", b"second").json();
    let old = first["generation"].as_str().unwrap();
    let live = second["generation"].as_str().unwrap();
    for condition in [
        format!("ifGenerationMatch={live}"),
        format!("ifGenerationNotMatch={old}"),
        "ifMetagenerationMatch=1".into(),
        "ifMetagenerationNotMatch=2".into(),
    ] {
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/test-bucket/o/file?generation={old}&{condition}"),
                "",
                b"",
            )
            .error(412, "conditionNotMet");
    }
    assert_eq!(server.get("file").json(), second);
    assert_eq!(
        server.get(&format!("file?generation={old}&alt=media")).body,
        b"first"
    );
    assert_eq!(blobs(&dir), 2);
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/file", "", b"")
            .status,
        204
    );
    server.get("file").error(404, "notFound");
    server
        .request("DELETE", "/storage/v1/b/test-bucket/o/file", "", b"")
        .error(404, "notFound");
    assert!(server.list("").get("items").is_none());
    assert_eq!(
        server
            .get(&format!("file?generation={live}&alt=media"))
            .body,
        b"second"
    );
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/test-bucket/o/file?generation={old}&ifGenerationMatch={old}&ifGenerationNotMatch=0&ifMetagenerationMatch=2&ifMetagenerationNotMatch=1"),
                "",
                b""
            )
            .status,
        204
    );
    server
        .get(&format!("file?generation={old}"))
        .error(404, "notFound");
    assert_eq!(blobs(&dir), 1);
    let third = server.put("file", b"third").json();
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!(
                    "/storage/v1/b/test-bucket/o/file?generation={}",
                    third["generation"].as_str().unwrap()
                ),
                "",
                b""
            )
            .status,
        204
    );
    server.get("file").error(404, "notFound");
    assert_eq!(
        server
            .get(&format!("file?generation={live}&alt=media"))
            .body,
        b"second"
    );
    assert_eq!(blobs(&dir), 1);
}

#[test]
fn versioned_listing_pages_and_query_boundaries() {
    let (_dir, server) = setup(false);
    let mut expected = Vec::new();
    for name in ["a", "dir%2F", "dir%2Fchild", "z"] {
        for _ in 0..3 {
            let value = server.put(name, b"body").json();
            expected.push((value["name"].clone(), value["generation"].clone()));
        }
    }
    let mut token = String::new();
    let mut actual = Vec::new();
    for _ in 0..20 {
        let page = server.list(&format!("versions=true&maxResults=1&pageToken={token}"));
        for item in page["items"].as_array().into_iter().flatten() {
            actual.push((item["name"].clone(), item["generation"].clone()));
        }
        let Some(next) = page["nextPageToken"].as_str() else {
            break;
        };
        server
            .request(
                "GET",
                &format!("/storage/v1/b/test-bucket/o?versions=false&pageToken={next}"),
                "",
                b"",
            )
            .error(400, "invalidParameter");
        token = next.into();
    }
    assert_eq!(actual, expected);
    let default_token = server.list("maxResults=1")["nextPageToken"]
        .as_str()
        .unwrap()
        .to_owned();
    server
        .request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o?versions=true&pageToken={default_token}"),
            "",
            b"",
        )
        .error(400, "invalidParameter");
    let mut token = String::new();
    let mut items = Vec::new();
    let mut prefixes = Vec::new();
    for _ in 0..20 {
        let page = server.list(&format!("versions=true&delimiter=%2F&includeTrailingDelimiter=true&maxResults=1&pageToken={token}"));
        items.extend(
            page["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|item| item["name"].as_str().unwrap().to_owned()),
        );
        prefixes.extend(
            page["prefixes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|item| item.as_str().unwrap().to_owned()),
        );
        let Some(next) = page["nextPageToken"].as_str() else {
            break;
        };
        token = next.into();
    }
    assert_eq!(
        items,
        ["a", "a", "a", "dir/", "dir/", "dir/", "z", "z", "z"]
    );
    assert_eq!(prefixes, ["dir/"]);
    assert_eq!(
        server.list("versions=true&prefix=dir%2F&startOffset=dir%2Fchild&endOffset=z")["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    for query in ["versions=1", "versions=true&versions=false"] {
        server
            .request(
                "GET",
                &format!("/storage/v1/b/test-bucket/o?{query}"),
                "",
                b"",
            )
            .error(400, "invalidParameter");
    }
    for value in ["", "abc", "9223372036854775808"] {
        server
            .get(&format!("a?generation={value}"))
            .error(400, "invalidParameter");
    }
    server.get("a?generation=0").error(404, "notFound");
}

#[test]
fn disabled_versioning_preserves_history_and_bucket_nonempty() {
    let (dir, server) = setup(false);
    let first = server.put("file", b"history").json();
    server.put("file", b"live").json();
    settings(&dir, false, false);
    server.put("file", b"replacement").json();
    assert_eq!(blobs(&dir), 2);
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/file", "", b"")
            .status,
        204
    );
    assert_eq!(blobs(&dir), 1);
    assert_eq!(
        server.list("versions=true")["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    server
        .request("DELETE", "/storage/v1/b/test-bucket", "", b"")
        .error(409, "conflict");
    assert_eq!(
        server
            .get(&format!(
                "file?generation={}&alt=media",
                first["generation"].as_str().unwrap()
            ))
            .body,
        b"history"
    );
}

#[test]
fn versioned_overwrite_delete_race() {
    let (_dir, server) = setup(false);
    for _ in 0..12 {
        let first = server.put("race", b"old").json();
        let generation = first["generation"].as_str().unwrap().to_owned();
        let barrier = Arc::new(Barrier::new(3));
        let handles:Vec<_>=[true,false].into_iter().map(|overwrite| {
            let barrier=barrier.clone(); let address=server.address.clone(); let generation=generation.clone();
            thread::spawn(move || {barrier.wait(); if overwrite {
                request(&address,"POST",&format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&name=race&ifGenerationMatch={generation}"),"",b"new").status
            } else {request(&address,"DELETE",&format!("/storage/v1/b/test-bucket/o/race?ifGenerationMatch={generation}"),"",b"").status}})
        }).collect();
        barrier.wait();
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(
            matches!(outcomes.as_slice(), [200, 412] | [412, 204]),
            "{outcomes:?}"
        );
        assert_eq!(
            server
                .get(&format!("race?generation={generation}&alt=media"))
                .body,
            b"old"
        );
    }
}

#[test]
fn versioned_strict_permissions() {
    let (_dir, server) = setup(true);
    let first = server.put("file", b"history").json();
    server.put("file", b"live").json();
    let generation = first["generation"].as_str().unwrap();
    let path = format!("/storage/v1/b/test-bucket/o/file?generation={generation}");
    assert_eq!(
        server
            .request(
                "GET",
                &format!("{path}&alt=media"),
                "Authorization: Bearer viewer\r\n",
                b""
            )
            .body,
        b"history"
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o?versions=true",
                "Authorization: Bearer viewer\r\n",
                b""
            )
            .status,
        200
    );
    for method in ["DELETE", "PATCH"] {
        server
            .request(
                method,
                &path,
                "Authorization: Bearer viewer\r\n",
                if method == "PATCH" { b"{}" } else { b"" },
            )
            .error(403, "forbidden");
    }
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=file",
            "Authorization: Bearer creator\r\n",
            b"forbidden",
        )
        .error(403, "forbidden");
}

#[test]
fn versioned_crash_recovery_preserves_referenced_generations() {
    let (dir, mut server) = setup(false);
    let first = server.put("file", b"history").json();
    let live = server.put("file", b"live").json();
    let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=file HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\nnew").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while blobs(&dir) < 3 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    drop(stream);
    db.execute_batch("ROLLBACK").unwrap();
    drop(db);
    drop(server);
    let mut server = Server::start(&dir, false);
    assert_eq!(blobs(&dir), 2);
    assert_eq!(server.get("file").json(), live);
    assert_eq!(
        server
            .get(&format!(
                "file?generation={}&alt=media",
                first["generation"].as_str().unwrap()
            ))
            .body,
        b"history"
    );
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/file", "", b"")
            .status,
        204
    );
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    drop(server);
    let mut server = Server::start(&dir, false);
    server.get("file").error(404, "notFound");
    assert_eq!(blobs(&dir), 2);
    let generation = first["generation"].as_str().unwrap();
    // A non-file blob path interrupts real reclamation after the deletion transaction commits.
    let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let id: String = db
        .query_row(
            "SELECT blob_id FROM objects WHERE generation=?1",
            [generation],
            |row| row.get(0),
        )
        .unwrap();
    let blob = dir.0.join("blobs").join(&id[..2]).join(&id);
    let saved = dir.0.join("interrupted-reclaim");
    std::fs::rename(&blob, &saved).unwrap();
    std::fs::create_dir(&blob).unwrap();
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/test-bucket/o/file?generation={generation}"),
                "",
                b""
            )
            .status,
        204
    );
    assert!(
        !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE generation=?1)",
            [generation],
            |row| row.get::<_, bool>(0)
        )
        .unwrap()
    );
    assert!(
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pending_blob_deletions WHERE blob_id=?1)",
            [&id],
            |row| row.get::<_, bool>(0)
        )
        .unwrap()
    );
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    drop(server);
    drop(db);
    std::fs::remove_dir(&blob).unwrap();
    std::fs::rename(&saved, &blob).unwrap();
    let server = Server::start(&dir, false);
    assert_eq!(blobs(&dir), 1);
    let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    server
        .get(&format!("file?generation={generation}"))
        .error(404, "notFound");
    assert_eq!(
        server
            .get(&format!(
                "file?generation={}&alt=media",
                live["generation"].as_str().unwrap()
            ))
            .body,
        b"live"
    );
}

#[tokio::test]
async fn versioned_schema_migrations_preserve_objects() {
    use gcs_mimic::{
        object::{ExpectedHashes, UploadChunk},
        storage::Catalog,
    };
    for version in [7, 8] {
        let (dir, mut server) = setup(false);
        server.put("file", b"bytes").json();
        let original=server.request("PATCH","/storage/v1/b/test-bucket/o/file","",br#"{"contentType":"text/plain","contentEncoding":"identity","contentDisposition":"inline","contentLanguage":"en","cacheControl":"private, max-age=10","metadata":{"team":"storage","empty":""}}"#).json();
        server.stop();
        drop(server);
        let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        let expected: (gcs_mimic::object::ObjectRecord, Vec<u8>) = {
            let catalog = Catalog::open(&dir.0).await.unwrap();
            let (mut record, mut reader) = catalog
                .admin(Origin::Seed)
                .open_object(
                    &BucketName::new("test-bucket").unwrap(),
                    &ObjectName::new("file").unwrap(),
                    None,
                )
                .await
                .unwrap();
            let mut bytes = Vec::new();
            while let Some(chunk) = reader.next_chunk().await.unwrap() {
                bytes.extend_from_slice(&chunk);
            }
            for time in [&mut record.time_created, &mut record.updated] {
                *time = object_effects_support::common::timestamp_support::floor_to_millis(*time);
            }
            catalog.close().await.unwrap();
            (record, bytes)
        };
        let last: i64 = db
            .query_row("SELECT last_generation FROM object_generation", [], |row| {
                row.get(0)
            })
            .unwrap();
        // An allocator above wall time detects reset even when timestamps would still increase.
        let future = last.checked_add(1_000_000_000_000).unwrap();
        db.execute("UPDATE object_generation SET last_generation=?1", [future])
            .unwrap();
        object_effects_support::common::downgrade_object_schema(&db);
        if version == 7 {
            db.execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold;").unwrap();
        }
        db.pragma_update(None, "user_version", version).unwrap();
        drop(db);
        for _ in 0..2 {
            let catalog = Catalog::open(&dir.0).await.unwrap();
            let record = catalog
                .admin(Origin::Seed)
                .get_object(
                    &BucketName::new("test-bucket").unwrap(),
                    &ObjectName::new("file").unwrap(),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(record, expected.0);
            assert_eq!(record.generation.to_string(), original["generation"]);
            assert!(!record.event_based_hold);
            assert!(record.time_deleted.is_none());
            let bucket = catalog
                .admin(Origin::Seed)
                .get_bucket(&BucketName::new("test-bucket").unwrap())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(bucket.versioning_enabled, version == 8);
            let (selected, mut reader) = catalog
                .admin(Origin::Seed)
                .open_object(
                    &BucketName::new("test-bucket").unwrap(),
                    &ObjectName::new("file").unwrap(),
                    None,
                )
                .await
                .unwrap();
            assert_eq!(selected, expected.0);
            let mut bytes = Vec::new();
            while let Some(chunk) = reader.next_chunk().await.unwrap() {
                bytes.extend_from_slice(&chunk);
            }
            assert_eq!(bytes, expected.1);
            catalog.close().await.unwrap();
            let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
            assert_eq!(
                db.query_row("SELECT last_generation FROM object_generation", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
                future
            );
        }
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        tx.send(Ok(UploadChunk::Data(bytes::Bytes::from_static(b"new"))))
            .await
            .unwrap();
        tx.send(Ok(UploadChunk::End)).await.unwrap();
        drop(tx);
        let next = catalog
            .admin(Origin::Seed)
            .publish_object(
                &BucketName::new("test-bucket").unwrap(),
                &ObjectName::new("file").unwrap(),
                Publication {
                    content_type: "text/plain".into(),
                    expected: ExpectedHashes::default(),
                    condition: None,
                    metadata: Default::default(),
                    max_bytes: 10,
                    mode: PublishMode::Replace,
                },
                rx,
            )
            .await
            .unwrap();
        assert_eq!(next.generation, future + 1);
        catalog.close().await.unwrap();
        assert_eq!(
            rusqlite::Connection::open(dir.0.join("catalog.sqlite3"))
                .unwrap()
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            31
        );
    }
}

#[test]
fn timestamp_migration_preserves_versioned_json_get_and_xml_head() {
    let (dir, mut server) = setup(false);
    let first = server.put("file", b"first").json();
    let second = server.put("file", b"second").json();
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/file", "", b"")
            .status,
        204
    );
    let third = server.put("file", b"live").json();
    server.stop();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE objects SET time_created = -1001000 WHERE generation = ?1",
            [first["generation"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()],
        )
        .unwrap();
    drop(connection);
    let mut server = Server::start(&dir, false);
    let generations: Vec<_> = [&first, &second, &third]
        .into_iter()
        .map(|value| value["generation"].as_str().unwrap().to_owned())
        .collect();
    let snapshot = |server: &Server| {
        generations
            .iter()
            .map(|generation| {
                let json = server.get(&format!("file?generation={generation}"));
                let head = server.request(
                    "HEAD",
                    &format!("/test-bucket/file?generation={generation}"),
                    "",
                    b"",
                );
                assert_eq!(json.status, 200);
                assert_eq!(head.status, 200);
                (
                    json.body,
                    json.headers["etag"].clone(),
                    head.headers["last-modified"].clone(),
                    head.headers["etag"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let before = snapshot(&server);
    assert!(String::from_utf8_lossy(&before[0].0).contains("1969-12-31T23:59:58.999Z"));
    server.stop();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    object_effects_support::common::downgrade_timestamp_precision(&connection);
    connection.pragma_update(None, "user_version", 13).unwrap();
    let deleted: Vec<i64> = connection
        .prepare(
            "SELECT time_deleted FROM objects WHERE time_deleted IS NOT NULL ORDER BY generation",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(deleted.len(), 2);
    drop(connection);
    let server = Server::start(&dir, false);
    assert_eq!(snapshot(&server), before);
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let migrated: Vec<i64> = connection
        .prepare(
            "SELECT time_deleted FROM objects WHERE time_deleted IS NOT NULL ORDER BY generation",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        migrated,
        deleted
            .into_iter()
            .map(|value| value * 1000)
            .collect::<Vec<_>>()
    );
}
