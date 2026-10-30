mod object_effects_support;

use object_effects_support::{Reply, Server, common::TestDir};
use serde_json::{Value, json};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const BUCKET: &str = "/storage/v1/b/test-bucket";

fn send(server: &Server, method: &str, path: &str, body: Value) -> Reply {
    server.request(method, path, ADMIN, body.to_string().as_bytes())
}

fn call(server: &Server, method: &str, path: &str) -> Reply {
    server.request(method, path, ADMIN, b"")
}

fn entries(value: &Value) -> Vec<(String, String)> {
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["entity"].as_str().unwrap().to_owned(),
                item["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(entity, role)| ((*entity).to_owned(), (*role).to_owned()))
        .collect()
}

const PROJECT_PRIVATE: &[(&str, &str)] = &[
    ("project-owners-0", "OWNER"),
    ("project-editors-0", "OWNER"),
    ("project-viewers-0", "READER"),
];

fn metageneration(value: &Value) -> i64 {
    value["metageneration"].as_str().unwrap().parse().unwrap()
}

#[test]
fn bucket_acl_crud_round_trips_and_advances_metageneration() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let acl = format!("{BUCKET}/acl");
    let list = call(&server, "GET", &acl).json();
    assert_eq!(list["kind"], "storage#bucketAccessControls");
    assert_eq!(entries(&list), pairs(PROJECT_PRIVATE));
    assert_eq!(
        list["items"][0],
        json!({
            "kind": "storage#bucketAccessControl", "id": "test-bucket/project-owners-0",
            "selfLink": "http://127.0.0.1:4443/storage/v1/b/test-bucket/acl/project-owners-0",
            "bucket": "test-bucket", "entity": "project-owners-0", "role": "OWNER",
            "projectTeam": {"projectNumber": "0", "team": "owners"}, "etag": "CAE="
        })
    );
    let before = metageneration(&call(&server, "GET", BUCKET).json());

    // Go ACL().Set uses update (PUT), which adds an entity that has no entry yet.
    let entry = send(
        &server,
        "PUT",
        &format!("{acl}/allAuthenticatedUsers"),
        json!({"bucket": "test-bucket", "entity": "allAuthenticatedUsers", "role": "READER"}),
    )
    .json();
    assert_eq!(entry["entity"], "allAuthenticatedUsers");
    assert_eq!(entry["role"], "READER");
    assert_eq!(entry["etag"], "CAI=");
    let bucket = call(&server, "GET", BUCKET).json();
    assert_eq!(metageneration(&bucket), before + 1);

    // Ruby add_reader uses insert (POST).
    let user = send(
        &server,
        "POST",
        &acl,
        json!({"entity": "user-liz@example.com", "role": "OWNER"}),
    )
    .json();
    assert_eq!(user["email"], "liz@example.com");
    assert_eq!(user["id"], "test-bucket/user-liz@example.com");
    let patched = send(
        &server,
        "PATCH",
        &format!("{acl}/allAuthenticatedUsers"),
        json!({"role": "WRITER"}),
    )
    .json();
    assert_eq!(patched["role"], "WRITER");
    let unchanged = send(
        &server,
        "PATCH",
        &format!("{acl}/allAuthenticatedUsers"),
        json!({}),
    )
    .json();
    assert_eq!(unchanged["role"], "WRITER");
    let got = call(&server, "GET", &format!("{acl}/user-liz%40example.com")).json();
    assert_eq!(got["role"], "OWNER");
    let mut expected = pairs(PROJECT_PRIVATE);
    expected.push(("allAuthenticatedUsers".into(), "WRITER".into()));
    expected.push(("user-liz@example.com".into(), "OWNER".into()));
    assert_eq!(entries(&call(&server, "GET", &acl).json()), expected);
    let fields = call(&server, "GET", &format!("{acl}?fields=items(entity)")).json();
    assert_eq!(
        fields["items"][3],
        json!({"entity": "allAuthenticatedUsers"})
    );

    let deleted = call(&server, "DELETE", &format!("{acl}/allAuthenticatedUsers"));
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    for method in ["GET", "DELETE"] {
        call(&server, method, &format!("{acl}/allAuthenticatedUsers")).error(404, "notFound");
    }
    assert_eq!(
        metageneration(&call(&server, "GET", BUCKET).json()),
        before + 5
    );
}

#[test]
fn acl_requests_reject_invalid_inputs_without_mutation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("object", b"x").json();
    let before = call(&server, "GET", BUCKET).json();
    for (method, path, body, status, reason) in [
        (
            "POST",
            "acl",
            json!({"entity": "allUsers"}),
            400,
            "required",
        ),
        ("POST", "acl", json!({"role": "READER"}), 400, "required"),
        (
            "POST",
            "acl",
            json!({"entity": "nobody", "role": "READER"}),
            400,
            "invalidArgument",
        ),
        (
            "POST",
            "acl",
            json!({"entity": "allUsers", "role": "reader"}),
            400,
            "invalidArgument",
        ),
        (
            "PUT",
            "acl/allUsers",
            json!({"entity": "allAuthenticatedUsers", "role": "READER"}),
            400,
            "invalidArgument",
        ),
        ("PUT", "acl/allUsers", json!({}), 400, "required"),
        ("PATCH", "acl/allUsers", json!({}), 400, "required"),
        (
            "POST",
            "defaultObjectAcl",
            json!({"entity": "allUsers", "role": "WRITER"}),
            400,
            "invalidArgument",
        ),
        (
            "POST",
            "o/object/acl",
            json!({"entity": "allUsers", "role": "WRITER"}),
            400,
            "invalidArgument",
        ),
        (
            "POST",
            "o/missing/acl",
            json!({"entity": "allUsers", "role": "READER"}),
            404,
            "notFound",
        ),
        (
            "POST",
            "o/object/acl?generation=1",
            json!({"entity": "allUsers", "role": "READER"}),
            404,
            "notFound",
        ),
        (
            "POST",
            "acl?userProject=p",
            json!({"entity": "allUsers", "role": "READER"}),
            400,
            "badRequest",
        ),
    ] {
        send(&server, method, &format!("{BUCKET}/{path}"), body).error(status, reason);
    }
    server
        .request("POST", &format!("{BUCKET}/acl"), ADMIN, b"{")
        .error(400, "parseError");
    server
        .request("GET", &format!("{BUCKET}/acl"), ADMIN, b"{}")
        .error(400, "invalidArgument");
    send(
        &server,
        "POST",
        "/storage/v1/b/missing-bucket/acl",
        json!({"entity": "allUsers", "role": "READER"}),
    )
    .error(404, "notFound");
    call(
        &server,
        "GET",
        "/storage/v1/b/missing-bucket/defaultObjectAcl",
    )
    .error(404, "notFound");
    assert_eq!(call(&server, "GET", BUCKET).json(), before);
    assert_eq!(
        entries(&call(&server, "GET", &format!("{BUCKET}/acl")).json()),
        pairs(PROJECT_PRIVATE)
    );
}

#[test]
fn default_object_acl_applies_only_to_objects_created_afterwards() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let default = format!("{BUCKET}/defaultObjectAcl");
    let list = call(&server, "GET", &default).json();
    assert_eq!(list["kind"], "storage#objectAccessControls");
    assert_eq!(entries(&list), pairs(PROJECT_PRIVATE));
    assert_eq!(
        list["items"][2],
        json!({
            "kind": "storage#objectAccessControl", "entity": "project-viewers-0", "role": "READER",
            "projectTeam": {"projectNumber": "0", "team": "viewers"}, "etag": "CAE="
        })
    );
    server.put("before", b"1").json();
    send(
        &server,
        "POST",
        &default,
        json!({"entity": "allUsers", "role": "READER"}),
    )
    .json();
    let current = metageneration(&call(&server, "GET", BUCKET).json());
    call(
        &server,
        "GET",
        &format!("{default}?ifMetagenerationMatch={}", current - 1),
    )
    .error(412, "conditionNotMet");
    assert_eq!(
        call(
            &server,
            "GET",
            &format!("{default}?ifMetagenerationNotMatch={current}")
        )
        .status,
        304
    );
    assert_eq!(
        entries(
            &call(
                &server,
                "GET",
                &format!("{default}?ifMetagenerationMatch={current}")
            )
            .json()
        )
        .len(),
        4
    );
    server.put("after", b"2").json();
    let mut inherited = pairs(PROJECT_PRIVATE);
    inherited.push(("allUsers".into(), "READER".into()));
    let after = call(&server, "GET", &format!("{BUCKET}/o/after/acl")).json();
    assert_eq!(entries(&after), inherited);
    assert_eq!(after["items"][3]["object"], "after");
    assert_eq!(
        entries(&call(&server, "GET", &format!("{BUCKET}/o/before/acl")).json()),
        pairs(PROJECT_PRIVATE)
    );
    // Changing the default later never rewrites existing objects.
    assert_eq!(
        call(&server, "DELETE", &format!("{default}/allUsers")).status,
        204
    );
    assert_eq!(
        entries(&call(&server, "GET", &format!("{BUCKET}/o/after/acl")).json()),
        inherited
    );
    // A replacement generation receives the current default, not the replaced object's ACL.
    send(
        &server,
        "PUT",
        &format!("{BUCKET}/o/after/acl/domain-example.com"),
        json!({"role": "OWNER"}),
    )
    .json();
    server.put("after", b"3").json();
    assert_eq!(
        entries(&call(&server, "GET", &format!("{BUCKET}/o/after/acl")).json()),
        pairs(PROJECT_PRIVATE)
    );
}

#[test]
fn object_acl_targets_a_generation_and_renders_with_full_projection() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let object = server.put("dir%2Fname", b"x").json();
    let generation = object["generation"].as_str().unwrap().to_owned();
    let acl = format!("{BUCKET}/o/dir%2Fname/acl");
    let entry = send(
        &server,
        "PUT",
        &format!("{acl}/allUsers?generation={generation}"),
        json!({"entity": "allUsers", "role": "READER"}),
    )
    .json();
    assert_eq!(
        entry["id"],
        format!("test-bucket/dir/name/{generation}/allUsers")
    );
    assert_eq!(
        entry["selfLink"],
        "http://127.0.0.1:4443/storage/v1/b/test-bucket/o/dir%2Fname/acl/allUsers"
    );
    assert_eq!(entry["generation"], generation.as_str());
    assert_eq!(entry["object"], "dir/name");
    let updated = call(&server, "GET", &format!("{BUCKET}/o/dir%2Fname")).json();
    assert_eq!(updated["metageneration"], "2");
    assert!(updated.get("acl").is_none());

    let full = call(
        &server,
        "GET",
        &format!("{BUCKET}/o/dir%2Fname?projection=full"),
    )
    .json();
    assert_eq!(full["acl"][3]["entity"], "allUsers");
    assert_eq!(full["acl"][3]["etag"], entry["etag"]);
    let listed = call(&server, "GET", &format!("{BUCKET}/o?projection=full")).json();
    assert_eq!(listed["items"][0]["acl"].as_array().unwrap().len(), 4);
    let selected = call(
        &server,
        "GET",
        &format!("{BUCKET}/o/dir%2Fname?projection=full&fields=acl(entity)"),
    )
    .json();
    assert_eq!(selected["acl"][3], json!({"entity": "allUsers"}));
    let patched = send(
        &server,
        "PATCH",
        &format!("{BUCKET}/o/dir%2Fname?projection=full"),
        json!({"contentType": "text/plain"}),
    )
    .json();
    assert_eq!(patched["acl"].as_array().unwrap().len(), 4);

    let bucket = call(&server, "GET", &format!("{BUCKET}?projection=full")).json();
    assert_eq!(bucket["acl"].as_array().unwrap().len(), 3);
    assert_eq!(bucket["defaultObjectAcl"][0]["entity"], "project-owners-0");
    assert_eq!(bucket["owner"], json!({"entity": "project-owners-0"}));
    let plain = call(&server, "GET", BUCKET).json();
    assert!(plain.get("acl").is_none() && plain.get("owner").is_none());
    let buckets = call(
        &server,
        "GET",
        "/storage/v1/b?project=local&projection=full",
    )
    .json();
    assert_eq!(buckets["items"][0]["acl"].as_array().unwrap().len(), 3);

    // ACLs persist across restarts and are removed with their generation.
    server.stop();
    let server = Server::start(&dir, false);
    let stored = call(&server, "GET", &acl).json();
    assert_eq!(entries(&stored).len(), 4);
    assert_eq!(
        call(&server, "DELETE", &format!("{BUCKET}/o/dir%2Fname")).status,
        204
    );
    call(&server, "GET", &acl).error(404, "notFound");
}

#[test]
fn uniform_access_and_public_access_prevention_restrict_acls() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("object", b"x").json();
    send(
        &server,
        "PUT",
        &format!("{BUCKET}/acl/domain-example.com"),
        json!({"role": "READER"}),
    )
    .json();
    send(
        &server,
        "PATCH",
        BUCKET,
        json!({"iamConfiguration": {"publicAccessPrevention": "enforced"}}),
    )
    .json();
    for path in [
        "acl/allUsers",
        "defaultObjectAcl/allAuthenticatedUsers",
        "o/object/acl/allUsers",
    ] {
        send(
            &server,
            "PUT",
            &format!("{BUCKET}/{path}"),
            json!({"role": "READER"}),
        )
        .error(412, "conditionNotMet");
    }
    send(
        &server,
        "PUT",
        &format!("{BUCKET}/o/object/acl/user-liz%40example.com"),
        json!({"role": "READER"}),
    )
    .json();

    let before = send(
        &server,
        "PATCH",
        BUCKET,
        json!({"iamConfiguration": {"uniformBucketLevelAccess": {"enabled": true}}}),
    )
    .json();
    for (method, path) in [
        ("GET", "acl"),
        ("GET", "acl/domain-example.com"),
        ("DELETE", "acl/domain-example.com"),
        ("GET", "defaultObjectAcl"),
        ("GET", "o/object/acl"),
    ] {
        call(&server, method, &format!("{BUCKET}/{path}")).error(400, "invalid");
    }
    for path in ["acl", "defaultObjectAcl", "o/object/acl"] {
        send(
            &server,
            "POST",
            &format!("{BUCKET}/{path}"),
            json!({"entity": "domain-example.com", "role": "READER"}),
        )
        .error(400, "invalid");
    }
    let full = call(&server, "GET", &format!("{BUCKET}?projection=full")).json();
    assert_eq!(metageneration(&full), metageneration(&before));
    assert!(full.get("acl").is_none() && full.get("defaultObjectAcl").is_none());
    assert_eq!(full["owner"], json!({"entity": "project-owners-0"}));
    let object = call(
        &server,
        "GET",
        &format!("{BUCKET}/o/object?projection=full"),
    )
    .json();
    assert!(object.get("acl").is_none());

    // Disabling uniform access again exposes the retained ACLs.
    send(
        &server,
        "PATCH",
        BUCKET,
        json!({"iamConfiguration": {"uniformBucketLevelAccess": {"enabled": false}}}),
    )
    .json();
    let acl = call(&server, "GET", &format!("{BUCKET}/acl")).json();
    assert_eq!(acl["items"][3]["entity"], "domain-example.com");
    let object = call(&server, "GET", &format!("{BUCKET}/o/object/acl")).json();
    assert_eq!(object["items"][3]["entity"], "user-liz@example.com");
}

#[test]
fn metadata_updates_replace_and_clear_acls() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("object", b"x").json();
    let object = format!("{BUCKET}/o/object");
    let full = call(&server, "GET", &format!("{object}?projection=full")).json();
    let mut body = full;
    body["contentType"] = json!("text/plain");
    let replaced = send(&server, "PUT", &object, body.clone()).json();
    assert_eq!(replaced["contentType"], "text/plain");
    assert_eq!(replaced["acl"].as_array().unwrap().len(), 3);
    // Reordered entries still describe the same ACL.
    let full = call(&server, "GET", &format!("{object}?projection=full")).json();
    let mut body = full;
    body["acl"].as_array_mut().unwrap().reverse();
    send(&server, "PATCH", &object, body).json();
    let bucket = call(&server, "GET", &format!("{BUCKET}?projection=full")).json();
    let mut body = bucket;
    body["labels"] = json!({"echo": "yes"});
    let updated = send(&server, "PUT", BUCKET, body).json();
    assert_eq!(updated["labels"], json!({"echo": "yes"}));
    for method in ["PATCH", "PUT"] {
        let before = metageneration(&call(&server, "GET", BUCKET).json());
        let acl = json!([{"entity":"allAuthenticatedUsers","role":"READER"}]);
        let default_acl = json!([{"entity":"domain-example.com","role":"OWNER"}]);
        let updated = send(
            &server,
            method,
            &format!("{BUCKET}?projection=full"),
            json!({
                "acl": acl, "defaultObjectAcl": default_acl, "labels":{"changed":"yes"}
            }),
        )
        .json();
        assert_eq!(updated["acl"][0]["entity"], "allAuthenticatedUsers");
        assert_eq!(
            updated["defaultObjectAcl"][0]["entity"],
            "domain-example.com"
        );
        assert_eq!(
            updated["labels"],
            if method == "PATCH" {
                json!({"changed":"yes", "echo":"yes"})
            } else {
                json!({"changed":"yes"})
            }
        );
        assert_eq!(metageneration(&updated), before + 1);
        server.put("inherited", b"x").json();
        assert_eq!(
            entries(&call(&server, "GET", &format!("{BUCKET}/o/inherited/acl")).json()),
            pairs(&[("domain-example.com", "OWNER")])
        );
        assert_eq!(
            entries(&call(&server, "GET", &format!("{object}/acl")).json()).len(),
            3
        );
        let before = metageneration(&call(&server, "GET", &object).json());
        let updated = send(
            &server,
            method,
            &format!("{object}?projection=full"),
            json!({
                "acl": acl, "contentType":"application/json", "metadata":{"key":"value"}
            }),
        )
        .json();
        assert_eq!(updated["acl"].as_array().unwrap().len(), 1);
        assert_eq!(updated["contentType"], "application/json");
        assert_eq!(metageneration(&updated), before + 1);
        // An omitted ACL stays unchanged even when PUT resets ordinary content metadata.
        let omitted = send(&server, "PUT", &object, json!({})).json();
        assert_eq!(omitted["acl"][0]["entity"], "allAuthenticatedUsers");
        assert!(omitted.get("metadata").is_none());
        let cleared = send(
            &server,
            method,
            &format!("{object}?projection=full"),
            json!({"acl":[]}),
        )
        .json();
        assert_eq!(cleared["acl"], json!([]));
        send(
            &server,
            method,
            BUCKET,
            json!({"acl":[],"defaultObjectAcl":[]}),
        )
        .json();
        for scope in ["acl", "defaultObjectAcl"] {
            assert!(entries(&call(&server, "GET", &format!("{BUCKET}/{scope}")).json()).is_empty());
        }
        server.put("empty-default", b"x").json();
        assert!(
            entries(&call(&server, "GET", &format!("{BUCKET}/o/empty-default/acl")).json())
                .is_empty()
        );
        // Existing objects keep their own stored ACL when the default changes.
        assert_eq!(
            entries(&call(&server, "GET", &format!("{BUCKET}/o/inherited/acl")).json()),
            pairs(&[("domain-example.com", "OWNER")])
        );
        send(&server, "PATCH", &object, json!({"acl": PROJECT_PRIVATE.iter().map(|(entity, role)| json!({"entity":entity,"role":role})).collect::<Vec<_>>()})).json();
    }
}

#[test]
fn resource_acl_validation_and_preconditions_preserve_all_metadata() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("object", b"x").json();
    for (path, field) in [
        (BUCKET.to_owned(), "acl"),
        (BUCKET.to_owned(), "defaultObjectAcl"),
        (format!("{BUCKET}/o/object"), "acl"),
    ] {
        let full = format!("{path}?projection=full");
        let before = call(&server, "GET", &full).json();
        for invalid in [
            json!(null),
            json!("private"),
            json!([{}]),
            json!([{"entity":"bad","role":"READER"}]),
            json!([{"entity":"allUsers","role":"reader"}]),
            json!([{"entity":"allUsers","role":"READER"},{"entity":"allUsers","role":"OWNER"}]),
            json!(
                (0..101)
                    .map(|i| json!({"entity":format!("user-{i}@example.com"),"role":"READER"}))
                    .collect::<Vec<_>>()
            ),
        ] {
            let mut body = if path.ends_with("/object") {
                json!({"contentType":"text/plain"})
            } else {
                json!({"labels":{"changed":"yes"}})
            };
            body[field] = invalid;
            for method in ["PATCH", "PUT"] {
                send(&server, method, &path, body.clone()).error(400, "invalidArgument");
                assert_eq!(call(&server, "GET", &full).json(), before);
            }
        }
        let mut body = if path.ends_with("/object") {
            json!({"contentType":"text/plain"})
        } else {
            json!({"labels":{"changed":"yes"}})
        };
        body[field] = json!([]);
        for condition in ["ifMetagenerationMatch=999", "ifMetagenerationNotMatch=1"] {
            send(
                &server,
                "PATCH",
                &format!("{path}?{condition}"),
                body.clone(),
            )
            .error(412, "conditionNotMet");
            assert_eq!(call(&server, "GET", &full).json(), before);
        }
    }
}

#[test]
fn resource_acl_policy_failures_roll_back_metadata_and_other_acls() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("object", b"x").json();
    let before = call(&server, "GET", &format!("{BUCKET}?projection=full")).json();
    for policy in [
        json!({"uniformBucketLevelAccess":{"enabled":true}}),
        json!({"publicAccessPrevention":"enforced"}),
    ] {
        let status = if policy.get("publicAccessPrevention").is_some() {
            412
        } else {
            400
        };
        let reason = if status == 412 {
            "conditionNotMet"
        } else {
            "invalidArgument"
        };
        send(&server, "PATCH", BUCKET, json!({"iamConfiguration":policy, "acl":[],
            "defaultObjectAcl":[{"entity":"allUsers","role":"READER"}], "labels":{"changed":"yes"}
        })).error(status, reason);
        assert_eq!(
            call(&server, "GET", &format!("{BUCKET}?projection=full")).json(),
            before
        );
    }
    for policy in [
        json!({"publicAccessPrevention":"enforced"}),
        json!({"uniformBucketLevelAccess":{"enabled":true}}),
    ] {
        send(&server, "PATCH", BUCKET, json!({"iamConfiguration":policy})).json();
        for (path, field) in [
            (BUCKET.to_owned(), "acl"),
            (BUCKET.to_owned(), "defaultObjectAcl"),
            (format!("{BUCKET}/o/object"), "acl"),
        ] {
            let full = format!("{path}?projection=full");
            let before = call(&server, "GET", &full).json();
            let mut body = if path.ends_with("/object") {
                json!({"contentType":"text/plain"})
            } else {
                json!({"labels":{"changed":"yes"}})
            };
            body[field] = json!([{"entity":"allUsers","role":"READER"}]);
            let (status, reason) = if policy.get("publicAccessPrevention").is_some() {
                (412, "conditionNotMet")
            } else {
                (400, "invalidArgument")
            };
            send(&server, "PATCH", &path, body).error(status, reason);
            assert_eq!(call(&server, "GET", &full).json(), before);
        }
    }
}

#[test]
fn resource_acl_generation_selection_and_conditioned_races() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    send(
        &server,
        "PATCH",
        BUCKET,
        json!({"versioning":{"enabled":true}}),
    )
    .json();
    let old = server.put("object", b"old").json();
    let live = server.put("object", b"new").json();
    let object = format!("{BUCKET}/o/object");
    let old_path = format!(
        "{object}?generation={}",
        old["generation"].as_str().unwrap()
    );
    send(
        &server,
        "PATCH",
        &old_path,
        json!({"acl":[], "contentType":"text/plain"}),
    )
    .json();
    assert!(
        entries(
            &call(
                &server,
                "GET",
                &format!(
                    "{object}/acl?generation={}",
                    old["generation"].as_str().unwrap()
                )
            )
            .json()
        )
        .is_empty()
    );
    let full = format!("{object}?projection=full");
    let before = call(&server, "GET", &full).json();
    send(
        &server,
        "PATCH",
        &format!(
            "{object}?ifGenerationMatch={}",
            old["generation"].as_str().unwrap()
        ),
        json!({"acl":[],"contentType":"text/plain"}),
    )
    .error(412, "conditionNotMet");
    assert_eq!(call(&server, "GET", &full).json(), before);
    assert_eq!(before["generation"], live["generation"]);
    for path in [BUCKET.to_owned(), object.clone()] {
        let full = format!("{path}?projection=full");
        let revision = metageneration(&call(&server, "GET", &full).json());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut threads = Vec::new();
        for name in ["first", "second"] {
            let barrier = barrier.clone();
            let address = server.address.clone();
            let path = format!("{path}?ifMetagenerationMatch={revision}");
            let mut body = if path.contains("/o/") {
                json!({"metadata":{"winner":name}})
            } else {
                json!({"labels":{"winner":name}})
            };
            body["acl"] = json!([{"entity":format!("user-{name}@example.com"),"role":"READER"}]);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                object_effects_support::request(
                    &address,
                    "PATCH",
                    &path,
                    ADMIN,
                    body.to_string().as_bytes(),
                )
            }));
        }
        let mut statuses = Vec::new();
        for thread in threads {
            statuses.push(thread.join().unwrap().status);
        }
        statuses.sort();
        assert_eq!(statuses, [200, 412]);
        let updated = call(&server, "GET", &full).json();
        let winner = if path.contains("/o/") {
            &updated["metadata"]["winner"]
        } else {
            &updated["labels"]["winner"]
        };
        assert_eq!(
            updated["acl"][0]["entity"],
            format!("user-{}@example.com", winner.as_str().unwrap())
        );
        assert_eq!(metageneration(&updated), revision + 1);
    }
    server.stop();
    let server = Server::start(&dir, false);
    assert!(
        entries(
            &call(
                &server,
                "GET",
                &format!(
                    "{object}/acl?generation={}",
                    old["generation"].as_str().unwrap()
                )
            )
            .json()
        )
        .is_empty()
    );
    assert_eq!(
        entries(&call(&server, "GET", &format!("{object}/acl")).json()).len(),
        1
    );
}

#[test]
fn acl_entry_limit_is_enforced() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let acl = format!("{BUCKET}/acl");
    for index in 3..100 {
        send(
            &server,
            "POST",
            &acl,
            json!({"entity": format!("user-u{index}@example.com"), "role": "READER"}),
        )
        .json();
    }
    send(
        &server,
        "POST",
        &acl,
        json!({"entity": "user-extra@example.com", "role": "READER"}),
    )
    .error(400, "invalid");
    // Updating an existing entity at the limit still succeeds.
    send(
        &server,
        "POST",
        &acl,
        json!({"entity": "user-u3@example.com", "role": "OWNER"}),
    )
    .json();
    assert_eq!(entries(&call(&server, "GET", &acl).json()).len(), 100);
}

#[test]
fn strict_profile_requires_iam_policy_permissions() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("object", b"x").json();
    let viewer = "Authorization: Bearer viewer\r\n";
    for path in ["acl", "defaultObjectAcl", "o/object/acl"] {
        server
            .request("GET", &format!("{BUCKET}/{path}"), viewer, b"")
            .error(403, "forbidden");
        server
            .request("GET", &format!("{BUCKET}/{path}"), "", b"")
            .error(401, "required");
        server
            .request(
                "GET",
                &format!("{BUCKET}/{path}"),
                "Authorization: Bearer unknown\r\n",
                b"",
            )
            .error(401, "authError");
        assert_eq!(
            entries(&call(&server, "GET", &format!("{BUCKET}/{path}")).json()).len(),
            3
        );
    }
    server
        .request(
            "POST",
            &format!("{BUCKET}/o/object/acl"),
            viewer,
            br#"{"entity":"allUsers","role":"READER"}"#,
        )
        .error(403, "forbidden");
    // A caller who may read the object but not its ACL still gets the resource without ACLs.
    let object = server
        .request(
            "GET",
            &format!("{BUCKET}/o/object?projection=full"),
            viewer,
            b"",
        )
        .json();
    assert!(object.get("acl").is_none());
    let object = call(
        &server,
        "GET",
        &format!("{BUCKET}/o/object?projection=full"),
    )
    .json();
    assert_eq!(object["acl"].as_array().unwrap().len(), 3);

    // An update-only caller gets the same denial for an echo and a changed ACL.
    server
        .request(
            "PUT",
            &format!("{BUCKET}/iam"),
            ADMIN,
            json!({"bindings": [{"role": "roles/storage.objectUser", "members": ["user:viewer@example.test"]}]})
                .to_string()
                .as_bytes(),
        )
        .json();
    let mut echoed = json!({"contentType": "text/plain", "acl": object["acl"].clone()});
    for method in ["PATCH", "PUT"] {
        server
            .request(
                method,
                &format!("{BUCKET}/o/object"),
                viewer,
                echoed.to_string().as_bytes(),
            )
            .error(403, "forbidden");
    }
    echoed.as_object_mut().unwrap().remove("acl");
    let updated = server
        .request(
            "PATCH",
            &format!("{BUCKET}/o/object"),
            viewer,
            echoed.to_string().as_bytes(),
        )
        .json();
    assert_eq!(updated["contentType"], "text/plain");
}
