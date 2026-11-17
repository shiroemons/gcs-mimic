use super::predefined_upload::{ADMIN_OWNER, pairs, stored};
use super::*;

fn update(server: &Server, method: &str, query: &str, body: &Value, headers: &str) -> Reply {
    server.request(
        method,
        &format!("{OBJECT}?{query}"),
        headers,
        body.to_string().as_bytes(),
    )
}

/// A metadata PATCH or PUT replaces the object's ACL with the canned entries and keeps the
/// owner's grant: Cloud Storage left only the owner after `predefinedAcl=private` on a PATCH
/// (case obj-unsupported-inputs, recorded 2026-11-17). A PUT declares the same parameter.
#[test]
fn metadata_writes_replace_the_acl_with_predefined_acl() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        server.put(b"bytes");
        let patched = update(
            &server,
            "PATCH",
            "predefinedAcl=publicRead",
            &json!({}),
            ADMIN,
        )
        .json_ok();
        assert_eq!(patched["metageneration"], "2", "{backend}");
        assert_eq!(
            stored(&server, "value"),
            pairs(&[ADMIN_OWNER, ("allUsers", "READER")]),
            "{backend}"
        );
        update(
            &server,
            "PUT",
            "predefinedAcl=private",
            &json!({"contentType":"text/plain"}),
            ADMIN,
        )
        .json_ok();
        assert_eq!(stored(&server, "value"), pairs(&[ADMIN_OWNER]), "{backend}");
        // No recording shows which one wins when a request names both.
        update(
            &server,
            "PATCH",
            "predefinedAcl=publicRead",
            &json!({"acl":[{"entity":"allUsers","role":"READER"}]}),
            ADMIN,
        )
        .json_reason(400, "badRequest");
        assert_eq!(stored(&server, "value"), pairs(&[ADMIN_OWNER]), "{backend}");
    }
}

/// Changing the ACL needs storage.objects.setIamPolicy as an `acl` body does, public access
/// prevention refuses a public canned ACL, and uniform access refuses any in Cloud Storage's
/// words (case bkt-gaps-settings, recorded 2026-11-17).
#[test]
fn predefined_acl_updates_need_set_iam_policy_and_respect_access_settings() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    server.put(b"bytes");
    let inherited = stored(&server, "value");
    server
        .admin(
            "PUT",
            &format!("{BUCKET}/iam"),
            json!({"bindings":[{"role":"roles/storage.objectUser","members":["user:reader@example.test"]}]}),
        )
        .json_ok();
    update(
        &server,
        "PATCH",
        "predefinedAcl=private",
        &json!({}),
        READER,
    )
    .json_reason(403, "forbidden");
    update(
        &server,
        "PATCH",
        "",
        &json!({"contentType":"text/plain"}),
        READER,
    )
    .json_ok();
    assert_eq!(stored(&server, "value"), inherited);

    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
        )
        .json_ok();
    update(
        &server,
        "PATCH",
        "predefinedAcl=publicRead",
        &json!({}),
        ADMIN,
    )
    .json_reason(412, "conditionNotMet");
    assert_eq!(stored(&server, "value"), inherited);

    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
        )
        .json_ok();
    let refused = update(&server, "PATCH", "predefinedAcl=private", &json!({}), ADMIN);
    refused.json_reason(400, "invalid");
    assert_eq!(
        serde_json::from_slice::<Value>(&refused.body).unwrap()["error"]["message"],
        "Cannot update access control for an object when uniform bucket-level access is enabled. Read more at https://cloud.google.com/storage/docs/uniform-bucket-level-access"
    );
}
