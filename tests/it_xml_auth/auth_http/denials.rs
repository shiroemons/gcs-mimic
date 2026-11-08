//! Strict denials name the refused principal and permission in JSON and XML, read the same for
//! a missing bucket as for an existing one, and appear in the debug log only.
use super::*;
use common::errors::{ANONYMOUS, denial_message, xml_error_message};

fn strict_with_bucket(dir: &TestDir, identities: &Path) -> Server {
    let server = Server::strict(dir, identities);
    server
        .create_bucket(ADMIN, "project-a", "bucket-a")
        .success();
    server.upload(ADMIN, "bucket-a", "o", b"data").success();
    server
}

#[test]
fn json_denials_name_the_principal_and_the_refused_permission() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = strict_with_bucket(&dir, &identity_file(&config));
    // roles/storage.objectViewer reads objects but holds no bucket, create or delete permission.
    let viewer = "viewer@example.invalid";
    server
        .get(VIEWER, "/storage/v1/b/bucket-a")
        .denied(viewer, "storage.buckets.get", "bucket");
    server.upload(VIEWER, "bucket-a", "new", b"x").denied(
        viewer,
        "storage.objects.create",
        "object",
    );
    server
        .request("DELETE", "/storage/v1/b/bucket-a/o/o", VIEWER, b"")
        .denied(viewer, "storage.objects.delete", "object");
    // roles/storage.objectCreator creates objects but can neither read nor replace one.
    let creator = "creator@example.invalid";
    server.get(CREATOR, "/storage/v1/b/bucket-a/o/o").denied(
        creator,
        "storage.objects.get",
        "object",
    );
    server.upload(CREATOR, "bucket-a", "o", b"x").denied(
        creator,
        "storage.objects.delete",
        "object",
    );
    // Project-level permissions name the project.
    let no_roles = "no-roles@example.invalid";
    server
        .create_bucket(NO_ROLES, "project-a", "denied-bucket")
        .denied(no_roles, "storage.buckets.create", "project");
    server
        .get(NO_ROLES, "/storage/v1/b?project=project-a")
        .denied(no_roles, "storage.buckets.list", "project");
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/o?alt=media")
            .success()
            .body,
        b"data"
    );
}

#[test]
fn a_denial_for_a_missing_bucket_or_object_reads_like_one_for_an_existing_resource() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = strict_with_bucket(&dir, &identity_file(&config));
    for path in ["/storage/v1/b/bucket-a", "/storage/v1/b/missing-bucket"] {
        server.get(NO_ROLES, path).denied(
            "no-roles@example.invalid",
            "storage.buckets.get",
            "bucket",
        );
        server
            .get("", path)
            .anonymous_denied("storage.buckets.get", "bucket");
    }
    for path in [
        "/storage/v1/b/bucket-a/o/o",
        "/storage/v1/b/bucket-a/o/missing",
        "/storage/v1/b/missing-bucket/o/o",
    ] {
        server.get(NO_ROLES, path).denied(
            "no-roles@example.invalid",
            "storage.objects.get",
            "object",
        );
        server
            .get("", path)
            .anonymous_denied("storage.objects.get", "object");
    }
    // A caller holding the permission in some project still learns that a bucket is missing.
    server
        .get(ADMIN, "/storage/v1/b/missing-bucket")
        .auth_error(404, "notFound");
    server
        .get(ADMIN, "/storage/v1/b/bucket-a/o/missing")
        .auth_error(404, "notFound");
}

#[test]
fn xml_denials_keep_their_codes_and_name_the_refused_permission() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = strict_with_bucket(&dir, &identity_file(&config));
    for path in ["/bucket-a/o", "/missing-bucket/o"] {
        xml_error_message(
            &server.get(NO_ROLES, path),
            403,
            "AccessDenied",
            &denial_message("no-roles@example.invalid", "storage.objects.get", "object"),
        );
        xml_error_message(
            &server.get("", path),
            401,
            "AuthenticationRequired",
            &denial_message(ANONYMOUS, "storage.objects.get", "object"),
        );
    }
    xml_error_message(
        &server.request("PUT", "/bucket-a/new", VIEWER, b"x"),
        403,
        "AccessDenied",
        &denial_message("viewer@example.invalid", "storage.objects.create", "object"),
    );
}

#[test]
fn denials_are_logged_at_debug_level_only_and_never_carry_credentials() {
    let config = TestDir::new();
    let identities = identity_file(&config);
    for level in ["debug", "info"] {
        let data = TestDir::new();
        let (mut server, _) = Server::start_with_stderr(
            &data,
            &[
                "--auth-mode",
                "strict",
                "--identity-file",
                identities.to_str().unwrap(),
                "--log-level",
                level,
            ],
            Stdio::piped(),
        );
        server
            .create_bucket(ADMIN, "project-a", "bucket-a")
            .success();
        server.get(VIEWER, "/storage/v1/b/bucket-a").forbidden();
        xml_error_message(
            &server.get("", "/bucket-a/o"),
            401,
            "AuthenticationRequired",
            &denial_message(ANONYMOUS, "storage.objects.get", "object"),
        );
        // testIamPermissions discards each refusal it evaluates, so it logs none.
        assert_eq!(
            server
                .get(
                    VIEWER,
                    "/storage/v1/b/bucket-a/iam/testPermissions?permissions=storage.buckets.get"
                )
                .success()
                .json(),
            json!({"kind": "storage#testIamPermissionsResponse", "permissions": []})
        );
        let denials: Vec<Value> = stderr_events(&mut server)
            .into_iter()
            .filter(|event| event["event"] == "authorization_denied")
            .collect();
        if level == "info" {
            assert!(denials.is_empty(), "{denials:?}");
            continue;
        }
        assert_eq!(
            denials,
            [
                json!({"event": "authorization_denied", "level": "debug",
                       "principal": "viewer@example.invalid", "anonymous": false,
                       "permission": "storage.buckets.get", "resource": "bucket",
                       "bucket": "bucket-a"}),
                json!({"event": "authorization_denied", "level": "debug", "anonymous": true,
                       "permission": "storage.objects.get", "resource": "object",
                       "bucket": "bucket-a"}),
            ]
        );
        for event in &denials {
            assert!(!event.to_string().contains("-token"), "{event}");
        }
    }
}
