mod object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::{Value, json};

const BUCKET: &str = "/storage/v1/b/test-bucket";
const ADMIN: &str = "Authorization: Bearer admin\r\n";

#[test]
fn acl_lists_and_deletes_send_bare_etags_and_reinsertions_follow_existing_entries() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"data").json();
    for suffix in ["acl", "defaultObjectAcl", "o/file/acl"] {
        let path = format!("{BUCKET}/{suffix}");
        let before = server.request("GET", &path, "", b"");
        assert!(!before.headers["etag"].starts_with('"'));
        let entry = format!("{path}/project-viewers-0");
        let deleted = server.request("DELETE", &entry, "", b"");
        assert_eq!(deleted.status, 204);
        assert!(!deleted.headers["etag"].starts_with('"'));
        server
            .request(
                "POST",
                &path,
                "",
                br#"{"entity":"project-viewers-0","role":"READER"}"#,
            )
            .json();
        let listed = server.request("GET", &path, "", b"").json();
        assert_eq!(
            listed["items"].as_array().unwrap().last().unwrap()["entity"],
            "project-viewers-0"
        );
        if suffix == "o/file/acl" {
            assert_eq!(
                listed["items"][2]["entity"],
                "user-local-user@gcs-mimic.invalid"
            );
            assert!(
                listed["items"][2]["selfLink"]
                    .as_str()
                    .unwrap()
                    .ends_with("/acl/user-local-user@gcs-mimic.invalid")
            );
        }
    }
    server.stop();
    let server = Server::start(&dir, false);
    let listed = server
        .request("GET", &format!("{BUCKET}/o/file/acl"), "", b"")
        .json();
    assert_eq!(listed["items"][3]["entity"], "project-viewers-0");
}

#[test]
fn object_updates_omit_acls_by_default_and_keep_explicit_full_projection() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("file", b"data").json();
    for method in ["PUT", "PATCH"] {
        for (query, full) in [
            ("", false),
            ("?projection=noAcl", false),
            ("?projection=full", true),
        ] {
            let value = server
                .request(method, &format!("{BUCKET}/o/file{query}"), "", b"{}")
                .json();
            assert_eq!(value.get("acl").is_some(), full);
            assert_eq!(value.get("owner").is_some(), full);
        }
    }
}

#[test]
fn bucket_versioning_keeps_explicit_false_across_restart_and_clears_null() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    assert!(
        server
            .request("GET", BUCKET, "", b"")
            .json()
            .get("versioning")
            .is_none()
    );
    for enabled in [true, false] {
        let value = server
            .request(
                "PATCH",
                &format!("{BUCKET}?fields=name,versioning"),
                "",
                json!({"versioning":{"enabled":enabled}})
                    .to_string()
                    .as_bytes(),
            )
            .json();
        assert_eq!(
            value,
            json!({"name":"test-bucket","versioning":{"enabled":enabled}})
        );
    }
    server.stop();
    let server = Server::start(&dir, false);
    assert_eq!(
        server.request("GET", BUCKET, "", b"").json()["versioning"],
        json!({"enabled":false})
    );
    let cleared = server
        .request("PATCH", BUCKET, "", br#"{"versioning":null}"#)
        .json();
    assert!(cleared.get("versioning").is_none());
}

#[test]
fn metadata_preconditions_and_invalid_acl_name_the_rejected_input() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let object = server.put("file", b"data").json();
    let failed = server.request(
        "GET",
        &format!("{BUCKET}/o/file"),
        "If-Match: \"other\"\r\n",
        b"",
    );
    failed.error(412, "conditionNotMet");
    assert_eq!(failed.headers["etag"], object["etag"].as_str().unwrap());
    let error: Value = serde_json::from_slice(&failed.body).unwrap();
    assert_eq!(error["error"]["errors"][0]["location"], "If-Match");
    assert_eq!(error["error"]["errors"][0]["locationType"], "header");
    let failed = server.request(
        "PATCH",
        &format!("{BUCKET}?predefinedAcl=invalid"),
        "",
        b"{}",
    );
    failed.error(400, "invalidParameter");
    let error: Value = serde_json::from_slice(&failed.body).unwrap();
    assert_eq!(error["error"]["errors"][0]["location"], "predefinedAcl");
    assert_eq!(error["error"]["errors"][0]["locationType"], "parameter");
    let failed = server.request(
        "POST",
        &format!("{BUCKET}/o/result/compose"),
        "",
        br#"{"sourceObjects":[{"name":"file","objectPreconditions":{"ifGenerationMatch":"1"}}]}"#,
    );
    failed.error(412, "conditionNotMet");
    let error: Value = serde_json::from_slice(&failed.body).unwrap();
    assert_eq!(error["error"]["errors"][0]["location"], "If-Match");
    assert_eq!(error["error"]["errors"][0]["locationType"], "header");
}

#[test]
fn missing_move_source_names_the_local_or_authenticated_principal() {
    for (strict, principal) in [
        (false, "local-user@gcs-mimic.invalid"),
        (true, "admin@example.test"),
    ] {
        let dir = TestDir::new();
        let server = Server::start(&dir, strict);
        server.bucket();
        let failed = server.request(
            "POST",
            &format!("{BUCKET}/o/missing/moveTo/o/target"),
            ADMIN,
            b"",
        );
        failed.error(403, "forbidden");
        let error: Value = serde_json::from_slice(&failed.body).unwrap();
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with(&format!(
                    "{principal} does not have storage.objects.move access"
                ))
        );
    }
}

#[test]
fn missing_move_source_uses_an_email_or_the_generic_permission_message() {
    for (id, alias, email) in [
        (
            "local:aliased",
            Some("user-alias@example.test"),
            Some("alias@example.test"),
        ),
        (
            "known@example.test",
            Some("user-123456"),
            Some("known@example.test"),
        ),
        ("local:aliasless", None, None),
        ("opaqueprincipal", None, None),
        ("local:numeric-alias", Some("user-123456"), None),
    ] {
        let dir = TestDir::new();
        let registry_path = dir.0.join("identities.json");
        let mut identity =
            json!({"id":id,"tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}});
        if let Some(alias) = alias {
            identity["acl_entity"] = alias.into();
        }
        std::fs::write(&registry_path, json!({"identities":[identity]}).to_string()).unwrap();
        let server = Server::start_with_args(
            &dir,
            &[
                "--auth-mode",
                "strict",
                "--identity-file",
                registry_path.to_str().unwrap(),
            ],
        );
        server.bucket();
        let failed = server.request(
            "POST",
            &format!("{BUCKET}/o/missing/moveTo/o/target"),
            ADMIN,
            b"",
        );
        failed.error(403, "forbidden");
        let error: Value = serde_json::from_slice(&failed.body).unwrap();
        let message = error["error"]["message"].as_str().unwrap();
        if let Some(email) = email {
            assert!(message.starts_with(&format!(
                "{email} does not have storage.objects.move access"
            )));
        } else {
            assert_eq!(message, "Forbidden");
            assert!(!message.contains(id));
        }
    }
}

#[test]
fn upload_front_end_errors_preserve_content_type_without_nosniff() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    for (query, headers, body) in [
        ("uploadType=media", "", "data"),
        (
            "uploadType=media&name=file&ifGenerationMatch=invalid",
            "",
            "data",
        ),
        ("uploadType=resumable", "", "{}"),
        (
            "uploadType=multipart",
            "Content-Type: multipart/related; boundary=b\r\n",
            "--b\r\nContent-Type: application/json\r\n\r\n{invalid\r\n--b\r\nContent-Type: text/plain\r\n\r\ndata\r\n--b--\r\n",
        ),
    ] {
        let failed = server.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?{query}"),
            headers,
            body.as_bytes(),
        );
        assert_eq!(failed.status, 400, "{query}");
        assert_eq!(failed.headers["content-type"], "text/html; charset=UTF-8");
        assert!(!failed.headers.contains_key("x-content-type-options"));
    }
    let failed = server.request("POST", "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=file&ifGenerationMatch=%3Cscript%3E%26alert(1)%3C%2Fscript%3E", "", b"data");
    failed.error(400, "invalidParameter");
    let text = String::from_utf8_lossy(&failed.body);
    assert!(!text.contains(['<', '>', '&']));
    let error: Value = serde_json::from_slice(&failed.body).unwrap();
    assert_eq!(
        error["error"]["message"],
        "Invalid long value: '<script>&alert(1)</script>'."
    );
}
