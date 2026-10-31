use super::*;
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

#[test]
fn object_owner_grants_metadata_and_acl_authority_only_on_its_selected_generation() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        let source = server.put(b"old generation bytes");
        let generation = source["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "READER", generation);
        let target = format!("{OBJECT}?generation={generation}");
        let acl = format!("{OBJECT}/acl?generation={generation}");
        assert_eq!(server.request("GET", &acl, READER, b"").status, 403);
        assert_eq!(server.request("PATCH", &target, READER, b"{}").status, 403);
        server.grant("user-reader@example.test", "OWNER", generation);
        server.put(b"private replacement");
        let changed = server
            .request("PATCH", &target, READER, br#"{"metadata":{"by":"owner"}}"#)
            .json();
        assert_eq!(changed["metadata"], json!({"by":"owner"}));
        let full = server
            .request("GET", &format!("{target}&projection=full"), READER, b"")
            .json();
        assert_eq!(full["owner"], json!({"entity":"user-admin@example.test"}));
        assert!(
            full["acl"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["entity"] == "user-reader@example.test"
                    && entry["role"] == "OWNER")
        );
        server
            .request("PUT", &target, READER, full.to_string().as_bytes())
            .json();
        server.request("GET", &acl, READER, b"").json();
        server
            .request(
                "POST",
                &acl,
                READER,
                br#"{"entity":"user-other@example.test","role":"OWNER"}"#,
            )
            .json();
        server
            .request(
                "PATCH",
                &target,
                OTHER,
                br#"{"metadata":{"by":"other-owner"}}"#,
            )
            .json();
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}/acl/user-other%40example.test?generation={generation}"),
                    READER,
                    b""
                )
                .status,
            204
        );
        assert_eq!(server.request("GET", &target, OTHER, b"").status, 403);
        for path in [
            OBJECT.into(),
            format!("{OBJECT}?generation=1&ifMetagenerationMatch=1"),
            format!("{BUCKET}/o"),
            format!("{BUCKET}/acl"),
            format!("{BUCKET}/defaultObjectAcl"),
        ] {
            assert_eq!(
                server.request("GET", &path, READER, b"").status,
                403,
                "{path}"
            );
        }
        assert_eq!(server.request("DELETE", &target, READER, b"").status, 403);
        assert_eq!(server.request("PATCH", BUCKET, READER, b"{}").status, 403);
        assert_eq!(
            server
                .request(
                    "PATCH",
                    &format!("{target}&ifMetagenerationMatch=1"),
                    READER,
                    b"{}"
                )
                .status,
            412
        );
        assert_eq!(server.request("PATCH", &target, READER, br#"{"retention":{"mode":"Unlocked","retainUntilTime":"2030-01-01T00:00:00Z"}}"#).status, 403);
        assert_eq!(server.request("POST", &format!("{BUCKET}/iam/testPermissions"), READER, br#"{"permissions":["storage.objects.getIamPolicy","storage.objects.update","storage.objects.create","storage.objects.delete"]}"#).status, 405);
        let probe = server.request("GET", &format!("{BUCKET}/iam/testPermissions?permissions=storage.objects.getIamPolicy&permissions=storage.objects.update&permissions=storage.objects.create&permissions=storage.objects.delete"), READER, b"").json();
        assert_eq!(probe["permissions"], json!([]));
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        assert_eq!(server.request("GET", &acl, READER, b"").status, 403);
        assert_eq!(server.request("PATCH", &target, READER, b"{}").status, 403);
        assert_eq!(
            server
                .request("GET", &format!("{target}&alt=media"), ADMIN, b"")
                .body,
            b"old generation bytes"
        );
    }
}

#[test]
fn object_acl_full_projection_rechecks_each_listed_generation_separately() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        let source = server.put(b"selected owner generation");
        let generation = source["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "OWNER", generation);
        super::bucket::grant(&server, "user-reader@example.test", "READER");
        server.put(b"private live generation");
        server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=other",
                ADMIN,
                b"another private object",
            )
            .json();
        let path = format!("{BUCKET}/o?versions=true&projection=full");
        let list = server.request("GET", &path, READER, b"").json();
        let items = list["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        for item in items {
            if item["name"] == "value" && item["generation"] == generation {
                assert!(item["acl"].is_array());
                assert_eq!(item["owner"], json!({"entity":"user-admin@example.test"}));
            } else {
                assert!(item.get("acl").is_none());
                assert!(item.get("owner").is_none());
            }
        }
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        let revoked = server.request("GET", &path, READER, b"").json();
        for item in revoked["items"].as_array().unwrap() {
            assert!(item.get("acl").is_none());
            assert!(item.get("owner").is_none());
        }
    }
}

#[test]
fn uploader_owner_authority_survives_bucket_revocation_and_obeys_uniform_access() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut server = Server::start(&data, &inputs, "disk");
    server.bucket();
    super::bucket::grant(&server, "user-reader@example.test", "WRITER");
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            READER,
            b"owned bytes",
        )
        .json();
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
    server
        .request(
            "PATCH",
            OBJECT,
            READER,
            br#"{"metadata":{"owner":"uploader"}}"#,
        )
        .json();
    let view = server
        .request("GET", &format!("{OBJECT}?projection=full"), READER, b"")
        .json();
    assert_eq!(view["owner"], json!({"entity":"user-reader@example.test"}));
    server
        .request("PUT", OBJECT, READER, view.to_string().as_bytes())
        .json();
    server
        .request(
            "POST",
            &format!("{OBJECT}/acl"),
            READER,
            br#"{"entity":"user-other@example.test","role":"READER"}"#,
        )
        .json();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
        )
        .json();
    server.request("PATCH", OBJECT, READER, b"{}").json();
    server.stop();
    server = Server::start(&data, &inputs, "disk");
    server
        .request("GET", &format!("{OBJECT}/acl"), READER, b"")
        .json();
    server.request("PATCH", OBJECT, READER, b"{}").json();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
        )
        .json();
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}/acl"), READER, b"")
            .status,
        403
    );
    assert_eq!(server.request("PATCH", OBJECT, READER, b"{}").status, 403);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}/acl"), ADMIN, b"")
            .status,
        400
    );
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}),
        )
        .json();
    server.request("PATCH", OBJECT, READER, b"{}").json();
    assert_eq!(server.request("DELETE", OBJECT, READER, b"").status, 403);
}

#[test]
fn object_acl_limit_counts_implicit_owner_and_rolls_back_oversized_publications() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    let mut entries: Vec<Value> = (0..99)
        .map(|index| json!({"entity":format!("user-member{index}@example.test"),"role":"READER"}))
        .collect();
    server
        .admin("PATCH", BUCKET, json!({"defaultObjectAcl":entries}))
        .json();
    let original = server.put(b"bounded ACL publication");
    let generation = original["generation"].as_str().unwrap();
    let path = format!("{OBJECT}?projection=full");
    let view = server.request("GET", &path, ADMIN, b"").json();
    assert_eq!(view["acl"].as_array().unwrap().len(), 100);
    assert_eq!(view["owner"], json!({"entity":"user-admin@example.test"}));
    let db = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    let stored: String = db
        .query_row(
            "SELECT entries FROM acl_object_entries WHERE name='value'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let raw: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 99);
    assert!(
        !raw.as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["entity"] == "user-admin@example.test")
    );
    let allocator: i64 = db
        .query_row("SELECT last_generation FROM object_generation", [], |row| {
            row.get(0)
        })
        .unwrap();
    entries.push(json!({"entity":"user-extra@example.test","role":"READER"}));
    server
        .admin("PATCH", BUCKET, json!({"defaultObjectAcl":entries}))
        .json();
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
                ADMIN,
                b"must not publish"
            )
            .status,
        400
    );
    assert_eq!(
        db.query_row("SELECT last_generation FROM object_generation", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        allocator
    );
    assert_eq!(server.request("GET", &path, ADMIN, b"").json(), view);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), ADMIN, b"")
            .body,
        b"bounded ACL publication"
    );
    assert_eq!(
        server.admin("PATCH", OBJECT, json!({"acl":entries})).status,
        400
    );
    assert_eq!(server.request("GET", &path, ADMIN, b"").json(), view);
    assert_eq!(
        server
            .admin(
                "POST",
                &format!("{OBJECT}/acl"),
                json!({"entity":"user-extra@example.test","role":"READER"})
            )
            .status,
        400
    );
    let owner_path = format!("{OBJECT}/acl/user-admin%40example.test?generation={generation}");
    assert_eq!(
        server
            .admin("PUT", &owner_path, json!({"role":"READER"}))
            .json()["role"],
        "OWNER"
    );
    assert_eq!(
        server.request("DELETE", &owner_path, ADMIN, b"").status,
        204
    );
    assert_eq!(
        server.request("GET", &path, ADMIN, b"").json()["acl"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    entries.pop();
    entries.push(json!({"entity":"user-admin@example.test","role":"READER"}));
    server.admin("PATCH", OBJECT, json!({"acl":entries})).json();
    let normalized = server.request("GET", &path, ADMIN, b"").json();
    assert_eq!(normalized["acl"].as_array().unwrap().len(), 100);
    assert!(
        normalized["acl"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["entity"] == "user-admin@example.test" && entry["role"] == "OWNER")
    );
}

#[test]
fn public_owner_roles_grant_metadata_and_acl_access_without_transferring_actual_ownership() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    let source = server.put(b"public OWNER role is not ownership");
    let generation = source["generation"].as_str().unwrap();
    let acl = format!("{OBJECT}/acl");
    server.grant("allAuthenticatedUsers", "OWNER", generation);
    assert_eq!(server.request("GET", &acl, "", b"").status, 401);
    server.request("GET", &acl, OTHER, b"").json();
    server
        .request(
            "PATCH",
            OBJECT,
            OTHER,
            br#"{"metadata":{"by":"registered"}}"#,
        )
        .json();
    assert_eq!(
        server
            .request("GET", &acl, "Authorization: Bearer unknown\r\n", b"")
            .status,
        401
    );
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("{acl}/allAuthenticatedUsers"),
                ADMIN,
                b""
            )
            .status,
        204
    );
    server.grant("allUsers", "OWNER", generation);
    let view = server
        .request("GET", &format!("{OBJECT}?projection=full"), "", b"")
        .json();
    assert_eq!(view["owner"], json!({"entity":"user-admin@example.test"}));
    server.request("GET", &acl, "", b"").json();
    server
        .request("PATCH", OBJECT, "", br#"{"metadata":{"by":"anonymous"}}"#)
        .json();
    assert_eq!(server.request("DELETE", OBJECT, "", b"").status, 401);
    server.grant("user-reader@example.test", "OWNER", generation);
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
        )
        .json();
    assert_eq!(server.request("GET", &acl, "", b"").status, 401);
    assert_eq!(server.request("PATCH", OBJECT, "", b"{}").status, 401);
    assert_eq!(server.request("PATCH", OBJECT, OTHER, b"{}").status, 403);
    server
        .request(
            "PATCH",
            OBJECT,
            READER,
            br#"{"metadata":{"explicit":"owner"}}"#,
        )
        .json();
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"inherited"}}),
        )
        .json();
    assert_eq!(
        server
            .request("DELETE", &format!("{acl}/allUsers"), "", b"")
            .status,
        204
    );
    assert_eq!(server.request("PATCH", OBJECT, "", b"{}").status, 401);
    assert_eq!(server.request("GET", &acl, "", b"").status, 401);
    let owner = server
        .request("GET", &format!("{OBJECT}?projection=full"), ADMIN, b"")
        .json();
    assert_eq!(owner["owner"], json!({"entity":"user-admin@example.test"}));
    assert_eq!(owner["metadata"]["explicit"], "owner");
}

#[test]
fn metadata_mutation_rechecks_owner_revocation_after_request_body_admission() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        let source = server.put(b"revocation keeps immutable bytes");
        let generation = source["generation"].as_str().unwrap();
        server.grant("user-reader@example.test", "OWNER", generation);
        let before = server.request("GET", OBJECT, ADMIN, b"").json();
        let mut stream = TcpStream::connect(&server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let body = br#"{"metadata":{"must":"not commit"}}"#;
        write!(stream, "PATCH {OBJECT}?ifMetagenerationMatch={} HTTP/1.1\r\nHost: localhost\r\n{READER}Connection: close\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n", before["metageneration"].as_str().unwrap(), body.len()).unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("HTTP/1.1 100 "), "{line}");
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
        }
        assert_eq!(
            server
                .request(
                    "DELETE",
                    &format!("{OBJECT}/acl/user-reader%40example.test?generation={generation}"),
                    ADMIN,
                    b""
                )
                .status,
            204
        );
        let revoked = server
            .request("GET", &format!("{OBJECT}?projection=full"), ADMIN, b"")
            .json();
        stream.write_all(body).unwrap();
        let mut reply = String::new();
        reader.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 403 "), "{reply}");
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?projection=full"), ADMIN, b"")
                .json(),
            revoked
        );
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), ADMIN, b"")
                .body,
            b"revocation keeps immutable bytes"
        );
    }
}
