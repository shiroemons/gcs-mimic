use super::*;

const OBJECT: &str = "/storage/v1/b/range-bucket/o/data";
const BUCKET: &str = "/storage/v1/b/range-bucket";

fn not_modified(server: &Server, path: &str, headers: &str, etag: &str, cache: &str) {
    let mut stream = server.open("GET", path, headers, b"");
    let reply = Reply::headers(&mut stream);
    assert_eq!(reply.status, 304, "{path}: {headers}");
    assert_eq!(reply.headers["etag"], etag);
    assert_eq!(reply.headers["cache-control"], cache);
    assert!(reply.headers.contains_key("date"));
    for name in [
        "content-type",
        "content-length",
        "content-range",
        "content-encoding",
    ] {
        assert!(!reply.headers.contains_key(name), "{name}");
    }
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).unwrap();
    assert!(bytes.is_empty(), "304 must not carry content: {bytes:?}");
}

#[test]
fn json_etag_read_object_metadata_media_and_range() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let original = server.setup("", b"0123456789");
    let etag = original["etag"].as_str().unwrap();
    for path in [
        OBJECT.to_owned(),
        format!("{OBJECT}?alt=json"),
        format!("{OBJECT}?alt=media"),
        format!("/download{OBJECT}?alt=media"),
    ] {
        let plain = server.request("GET", &path, "", b"");
        assert_eq!(plain.headers["etag"], etag);
        if !path.ends_with("alt=media") {
            let resource: Value = serde_json::from_slice(&plain.body).unwrap();
            assert_eq!(resource["etag"], etag);
        }
        for header in [
            format!("If-Match: {etag}\r\n"),
            "If-Match: *\r\n".to_owned(),
            format!("If-Match: \"other\", {etag}\r\n"),
            format!("If-Match: \"other\"\r\nIf-Match: {etag}\r\n"),
            "If-None-Match: \"other\"\r\n".to_owned(),
        ] {
            let reply = server.request("GET", &path, &header, b"");
            assert_eq!(reply.status, 200, "{path}: {header}");
            assert_eq!(reply.headers["etag"], etag);
            assert_eq!(reply.body, plain.body);
        }
        for header in [
            format!("If-None-Match: {etag}\r\n"),
            format!("If-None-Match: W/{etag}\r\n"),
            "If-None-Match: *\r\n".to_owned(),
            format!("If-None-Match: \"other\", W/{etag}\r\n"),
            format!("If-None-Match: \"other\"\r\nIf-None-Match: {etag}\r\n"),
            format!("If-Match: {etag}\r\nIf-None-Match: {etag}\r\n"),
        ] {
            not_modified(&server, &path, &header, etag, "no-cache");
        }
        for header in [
            "If-Match: \"other\"\r\n".to_owned(),
            format!("If-Match: W/{etag}\r\n"),
            format!("If-Match: \"other\"\r\nIf-None-Match: {etag}\r\n"),
        ] {
            server
                .request("GET", &path, &header, b"")
                .error(412, "conditionNotMet");
        }
    }
    for range in ["bytes=2-5", "bytes=100-", "nonsense"] {
        for prefix in ["", "/download"] {
            let path = format!("{prefix}{OBJECT}?alt=media");
            not_modified(
                &server,
                &path,
                &format!("Range: {range}\r\nIf-None-Match: {etag}\r\n"),
                etag,
                "no-cache",
            );
            server
                .request(
                    "GET",
                    &path,
                    &format!("Range: {range}\r\nIf-Match: \"other\"\r\n"),
                    b"",
                )
                .error(412, "conditionNotMet");
        }
    }
    let partial = server.get("", &format!("Range: bytes=2-5\r\nIf-Match: {etag}\r\n"));
    assert_eq!(partial.status, 206);
    assert_eq!(partial.headers["etag"], etag);
    assert_eq!(partial.body, b"2345");
    server.stop();
}

#[test]
fn json_etag_read_syntax_and_missing_resources() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.setup("", b"data");
    for path in [
        OBJECT.to_owned(),
        format!("{OBJECT}?alt=media"),
        BUCKET.to_owned(),
    ] {
        for header in ["If-Match", "If-None-Match"] {
            for value in [
                "unquoted",
                "W/unquoted",
                "w/\"weak\"",
                "\"open",
                "\"has space\"",
                "*, \"other\"",
                "*,*",
                "\"other\", bad",
            ] {
                server
                    .request("GET", &path, &format!("{header}: {value}\r\n"), b"")
                    .error(400, "invalidParameter");
            }
            server
                .request(
                    "GET",
                    &path,
                    &format!("{header}: *\r\n{header}: *\r\n"),
                    b"",
                )
                .error(400, "invalidParameter");
        }
        // Empty #entity-tag lists are valid: no member matches the representation.
        server
            .request("GET", &path, "If-Match: , ,\r\n", b"")
            .error(412, "conditionNotMet");
        assert_eq!(
            server
                .request("GET", &path, "If-None-Match: \r\n", b"")
                .status,
            200
        );
    }
    for path in [
        "/storage/v1/b/range-bucket/o/missing",
        "/storage/v1/b/range-bucket/o/missing?alt=media",
        "/storage/v1/b/missing-bucket",
    ] {
        for header in [
            "If-Match: *\r\n",
            "If-None-Match: *\r\n",
            "If-Match: \"other\"\r\n",
            "If-Match: malformed\r\n",
        ] {
            server
                .request("GET", path, header, b"")
                .error(404, "notFound");
        }
    }
    server.stop();
}

#[test]
fn json_etag_read_buckets_and_metadata_changes_in_both_profiles() {
    for legacy in [false, true] {
        let dir = TestDir::new();
        let mut server = Server::start_with_profile(&dir, false, legacy);
        server.setup("", b"data");
        let initial = server.request("GET", BUCKET, "", b"");
        let resource: Value = serde_json::from_slice(&initial.body).unwrap();
        let etag = &initial.headers["etag"];
        if legacy {
            assert_eq!(resource["etag"], "RVRhZw==");
            assert!(etag.starts_with("\"b-"));
        } else {
            assert_eq!(resource["etag"], etag.as_str());
        }
        not_modified(
            &server,
            BUCKET,
            &format!("If-None-Match: W/{etag}\r\n"),
            etag,
            "no-cache",
        );
        assert_eq!(
            server
                .request("GET", BUCKET, &format!("If-Match: {etag}\r\n"), b"")
                .status,
            200
        );
        server
            .request("GET", BUCKET, "If-Match: \"stale\"\r\n", b"")
            .error(412, "conditionNotMet");
        let changed = server.request("PATCH", BUCKET, "", br#"{"labels":{"state":"new"}}"#);
        assert_eq!(changed.status, 200);
        let updated = server.request("GET", BUCKET, &format!("If-None-Match: {etag}\r\n"), b"");
        assert_eq!(updated.status, 200);
        assert_ne!(updated.headers["etag"], *etag);
        server
            .request("GET", BUCKET, &format!("If-Match: {etag}\r\n"), b"")
            .error(412, "conditionNotMet");
        assert_eq!(server.request("DELETE", OBJECT, "", b"").status, 204);
        assert!(matches!(
            server.request("DELETE", BUCKET, "", b"").status,
            200 | 204
        ));
        assert_eq!(
            server
                .request(
                    "POST",
                    "/storage/v1/b?project=local-project",
                    "",
                    br#"{"name":"range-bucket"}"#,
                )
                .status,
            200
        );
        let recreated = server.request(
            "GET",
            &format!("{BUCKET}?fields=name"),
            &format!("If-None-Match: {etag}\r\n"),
            b"",
        );
        assert_eq!(recreated.status, 200);
        assert_eq!(
            serde_json::from_slice::<Value>(&recreated.body).unwrap(),
            serde_json::json!({"name":"range-bucket"})
        );
        assert_ne!(recreated.headers["etag"], *etag);
        server
            .request("GET", BUCKET, &format!("If-Match: {etag}\r\n"), b"")
            .error(412, "conditionNotMet");
        server.stop();
    }
}

#[test]
fn json_etag_read_selected_generation_zero_byte_and_public_cache() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let original = server.setup("", b"original");
    let etag = original["etag"].as_str().unwrap();
    let generation = original["generation"].as_str().unwrap();
    assert_eq!(
        server
            .request("PATCH", BUCKET, "", br#"{"versioning":{"enabled":true}}"#)
            .status,
        200
    );
    let replacement = server.upload("", b"");
    let empty_etag = replacement["etag"].as_str().unwrap();
    assert_ne!(etag, empty_etag);
    for alt in ["json", "media"] {
        let selected = format!("{OBJECT}?alt={alt}&generation={generation}");
        let reply = server.request("GET", &selected, &format!("If-Match: {etag}\r\n"), b"");
        assert_eq!(reply.status, 200);
        assert_eq!(reply.headers["etag"], etag);
        if alt == "media" {
            assert_eq!(reply.body, b"original");
        }
        not_modified(
            &server,
            &selected,
            &format!("If-None-Match: {etag}\r\n"),
            etag,
            "no-cache",
        );
        server
            .request(
                "GET",
                &selected,
                &format!("If-Match: {empty_etag}\r\n"),
                b"",
            )
            .error(412, "conditionNotMet");
    }
    assert_eq!(
        server.get("", &format!("If-Match: {empty_etag}\r\n")).body,
        b""
    );
    not_modified(
        &server,
        &format!("{OBJECT}?alt=media"),
        &format!("If-None-Match: {empty_etag}\r\nRange: bytes=0-1\r\n"),
        empty_etag,
        "no-cache",
    );
    assert_eq!(
        server
            .request(
                "PATCH",
                OBJECT,
                "",
                br#"{"cacheControl":"public, max-age=60"}"#
            )
            .status,
        200
    );
    assert_eq!(server.request("PUT", &format!("{BUCKET}/iam"), "", br#"{"version":1,"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#).status, 200);
    let public = server.get("", "");
    not_modified(
        &server,
        &format!("{OBJECT}?alt=media"),
        &format!("If-None-Match: {}\r\n", public.headers["etag"]),
        &public.headers["etag"],
        "public, max-age=60",
    );
    server.stop();
}

#[test]
fn json_etag_read_authorization_precedes_validators() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, true);
    server.setup("Authorization: Bearer local-admin-token\r\n", b"data");
    for path in [
        OBJECT.to_owned(),
        format!("{OBJECT}?alt=media"),
        BUCKET.to_owned(),
    ] {
        for header in [
            "If-None-Match: *\r\n",
            "If-Match: \"stale\"\r\n",
            "If-Match: malformed\r\n",
        ] {
            server
                .request("GET", &path, header, b"")
                .error(401, "required");
            server
                .request(
                    "GET",
                    &path,
                    &format!("Authorization: Bearer local-creator-token\r\n{header}"),
                    b"",
                )
                .error(403, "forbidden");
        }
    }
    server.stop();
}

#[test]
fn json_etag_read_unsupported_lists_dates_and_writes() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.setup("", b"data");
    for path in [
        OBJECT.to_owned(),
        format!("{OBJECT}?alt=media"),
        BUCKET.to_owned(),
    ] {
        for name in ["If-Modified-Since", "If-Unmodified-Since", "If-Range"] {
            server
                .request(
                    "GET",
                    &path,
                    &format!("{name}: Wed, 21 Oct 2015 07:28:00 GMT\r\n"),
                    b"",
                )
                .error(501, "notImplemented");
        }
    }
    for path in [
        format!("{BUCKET}/o"),
        "/storage/v1/b?project=local-project".into(),
    ] {
        for header in ["If-Match: *\r\n", "If-None-Match: *\r\n"] {
            server
                .request("GET", &path, header, b"")
                .error(501, "notImplemented");
        }
    }
    for (method, path, body) in [
        ("PATCH", OBJECT, b"{}".as_slice()),
        ("PUT", OBJECT, b"{}".as_slice()),
        ("DELETE", OBJECT, b"".as_slice()),
        (
            "POST",
            "/upload/storage/v1/b/range-bucket/o?uploadType=media&name=data",
            b"replacement".as_slice(),
        ),
        ("PATCH", BUCKET, b"{}".as_slice()),
        ("PUT", BUCKET, b"{}".as_slice()),
        ("DELETE", BUCKET, b"".as_slice()),
        (
            "POST",
            "/storage/v1/b?project=local-project",
            br#"{"name":"another-bucket"}"#.as_slice(),
        ),
    ] {
        for header in ["If-Match: *\r\n", "If-None-Match: *\r\n"] {
            server
                .request(method, path, header, body)
                .error(501, "notImplemented");
        }
    }
    assert_eq!(server.get("", "").body, b"data");
    server.stop();
}

#[test]
fn json_etag_read_combined_query_conditions_use_one_selected_record() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let object = server.setup("", b"data");
    let generation: i64 = object["generation"].as_str().unwrap().parse().unwrap();
    for (path, conditions, success) in [
        (
            format!("{OBJECT}?alt=json"),
            vec![
                ("ifGenerationMatch", generation, 0),
                ("ifGenerationNotMatch", generation + 1, generation),
                ("ifMetagenerationMatch", 1, 2),
                ("ifMetagenerationNotMatch", 2, 1),
            ],
            200,
        ),
        (
            format!("{OBJECT}?alt=media"),
            vec![
                ("ifGenerationMatch", generation, 0),
                ("ifGenerationNotMatch", generation + 1, generation),
                ("ifMetagenerationMatch", 1, 2),
                ("ifMetagenerationNotMatch", 2, 1),
            ],
            206,
        ),
        (
            format!("{BUCKET}?fields=name"),
            vec![
                ("ifMetagenerationMatch", 1, 2),
                ("ifMetagenerationNotMatch", 2, 1),
            ],
            200,
        ),
    ] {
        let etag = server.request("GET", &path, "", b"").headers["etag"].clone();
        for (key, passing, failing) in &conditions {
            for (headers, expected) in [
                (format!("If-Match: {etag}\r\n"), success),
                ("If-Match: \"stale\"\r\n".into(), 412),
                (format!("If-None-Match: {etag}\r\n"), 304),
                ("If-None-Match: \"stale\"\r\n".into(), success),
                (
                    format!("If-Match: \"stale\"\r\nIf-None-Match: {etag}\r\n"),
                    412,
                ),
                (
                    format!("If-Match: {etag}\r\nIf-None-Match: {etag}\r\n"),
                    304,
                ),
            ] {
                let headers = format!("{headers}Range: bytes=0-1\r\n");
                let reply =
                    server.request("GET", &format!("{path}&{key}={passing}"), &headers, b"");
                assert_eq!(reply.status, expected, "{path} {key}={passing} {headers}");
                if expected == 304 {
                    assert!(reply.body.is_empty());
                    assert_eq!(reply.headers["etag"], etag);
                } else if expected == 206 {
                    assert_eq!(reply.body, b"da");
                    assert_eq!(reply.headers["etag"], etag);
                }
                server
                    .request("GET", &format!("{path}&{key}={failing}"), &headers, b"")
                    .error(412, "conditionNotMet");
            }
        }
        let all = conditions
            .iter()
            .map(|(key, passing, _)| format!("&{key}={passing}"))
            .collect::<String>();
        let reply = server.request(
            "GET",
            &format!("{path}{all}"),
            &format!("If-Match: {etag}\r\n"),
            b"",
        );
        assert_eq!(reply.status, 200);
    }
    server.stop();
}

#[test]
fn json_etag_read_iam_policy_validators_preserve_fields_and_permissions() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, true);
    let admin = "Authorization: Bearer local-admin-token\r\n";
    server.setup(admin, b"data");
    let path = format!("{BUCKET}/iam");
    let initial = server.request("GET", &path, admin, b"");
    assert_eq!(initial.status, 200);
    let policy: Value = serde_json::from_slice(&initial.body).unwrap();
    let etag = &initial.headers["etag"];
    assert_eq!(*etag, format!("\"{}\"", policy["etag"].as_str().unwrap()));
    let selected = format!("{path}?fields=version");
    let reply = server.request(
        "GET",
        &selected,
        &format!("{admin}If-Match: {etag}\r\n"),
        b"",
    );
    assert_eq!(reply.status, 200);
    assert_eq!(
        serde_json::from_slice::<Value>(&reply.body).unwrap(),
        serde_json::json!({"version":1})
    );
    not_modified(
        &server,
        &selected,
        &format!("{admin}If-None-Match: W/{etag}\r\n"),
        etag,
        "no-cache",
    );
    server
        .request(
            "GET",
            &selected,
            &format!("{admin}If-Match: \"stale\"\r\n"),
            b"",
        )
        .error(412, "conditionNotMet");
    server
        .request(
            "GET",
            &path,
            &format!("{admin}If-Match: malformed\r\n"),
            b"",
        )
        .error(400, "invalidParameter");
    server
        .request("GET", &path, "If-None-Match: *\r\n", b"")
        .error(401, "required");
    server
        .request(
            "GET",
            &path,
            "Authorization: Bearer local-creator-token\r\nIf-None-Match: *\r\n",
            b"",
        )
        .error(403, "forbidden");
    server
        .request(
            "GET",
            "/storage/v1/b/missing-bucket/iam",
            &format!("{admin}If-Match: malformed\r\n"),
            b"",
        )
        .error(404, "notFound");
    let updated = server.request("PUT", &path, admin, br#"{"bindings":[]}"#);
    assert_eq!(updated.status, 200);
    assert_eq!(
        server
            .request(
                "GET",
                &path,
                &format!("{admin}If-None-Match: {etag}\r\n"),
                b""
            )
            .status,
        200
    );
    server
        .request("GET", &path, &format!("{admin}If-Match: {etag}\r\n"), b"")
        .error(412, "conditionNotMet");
    server.stop();
}

#[test]
fn json_etag_read_date_precedence_and_unrelated_routes_preserve_behavior() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let original = server.setup("", b"data");
    let etag = original["etag"].as_str().unwrap();
    let modified = "If-Modified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\n";
    for path in [
        OBJECT.to_owned(),
        format!("{OBJECT}?alt=media"),
        BUCKET.to_owned(),
    ] {
        let initial = server.request("GET", &path, "", b"");
        let selected = &initial.headers["etag"];
        not_modified(
            &server,
            &path,
            &format!("If-None-Match: {selected}\r\n{modified}"),
            selected,
            "no-cache",
        );
        let unmatched = server.request(
            "GET",
            &path,
            &format!("If-None-Match: \"other\"\r\n{modified}"),
            b"",
        );
        assert_eq!(unmatched.status, 200);
        assert_eq!(unmatched.body, initial.body);
    }
    for header in [
        modified,
        "If-Unmodified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\n",
        "If-Range: ignored\r\n",
    ] {
        let uploaded = server.request(
            "POST",
            "/upload/storage/v1/b/range-bucket/o?uploadType=media&name=other",
            header,
            b"other",
        );
        assert_eq!(uploaded.status, 200);
        for path in [
            format!("{BUCKET}/o"),
            "/storage/v1/b?project=local-project".to_owned(),
        ] {
            assert_eq!(server.request("GET", &path, header, b"").status, 200);
        }
    }
    assert_eq!(
        server.get("", &format!("If-Match: {etag}\r\n")).body,
        b"data"
    );
    server.stop();
}

#[test]
fn json_etag_read_if_match_overrides_if_unmodified_since() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.setup("", b"data");
    let unmodified = "If-Unmodified-Since: Wed, 21 Oct 2015 07:28:00 GMT\r\n";
    for path in [
        OBJECT.to_owned(),
        format!("{OBJECT}?alt=media"),
        BUCKET.to_owned(),
    ] {
        let initial = server.request("GET", &path, "", b"");
        let selected = &initial.headers["etag"];
        let matched = server.request(
            "GET",
            &path,
            &format!("If-Match: {selected}\r\n{unmodified}"),
            b"",
        );
        assert_eq!(matched.status, 200);
        assert_eq!(matched.body, initial.body);
        server
            .request(
                "GET",
                &path,
                &format!("If-Match: \"other\"\r\n{unmodified}"),
                b"",
            )
            .error(412, "conditionNotMet");
        server
            .request("GET", &path, unmodified, b"")
            .error(501, "notImplemented");
    }
    server.stop();
}

#[test]
fn json_etag_read_metadata_update_and_overwrite_pin_open_reader() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let original = server.setup("", &vec![b'a'; 8 * 1024 * 1024]);
    let etag = original["etag"].as_str().unwrap();
    let mut stream = server.open(
        "GET",
        &format!("{OBJECT}?alt=media"),
        &format!("If-Match: {etag}\r\nRange: bytes=1-\r\n"),
        b"",
    );
    let mut reply = Reply::headers(&mut stream);
    assert_eq!(reply.status, 206);
    let metadata = server.request("PATCH", OBJECT, "", br#"{"metadata":{"state":"changed"}}"#);
    assert_eq!(metadata.status, 200);
    let changed: Value = serde_json::from_slice(&metadata.body).unwrap();
    assert_ne!(changed["etag"], original["etag"]);
    server
        .get("", &format!("If-Match: {etag}\r\n"))
        .error(412, "conditionNotMet");
    server.upload("", b"replacement");
    reply.read_body(&mut stream);
    assert_eq!(reply.headers["etag"], etag);
    assert_eq!(
        reply.headers["x-goog-generation"],
        original["generation"].as_str().unwrap()
    );
    assert_eq!(reply.headers["x-goog-metageneration"], "1");
    assert_eq!(reply.body.len(), 8 * 1024 * 1024 - 1);
    assert!(reply.body.iter().all(|byte| *byte == b'a'));
    assert_eq!(server.get("", "").body, b"replacement");
    server.stop();
}
