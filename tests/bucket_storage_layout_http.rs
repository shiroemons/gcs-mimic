//! `buckets.getStorageLayout`: the resource, its parameters and the `storage.objects.list`
//! permission the reference names for it.

mod object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::{Value, json};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

fn layout(
    server: &Server,
    bucket: &str,
    query: &str,
    headers: &str,
) -> object_effects_support::Reply {
    server.request(
        "GET",
        &format!("/storage/v1/b/{bucket}/storageLayout{query}"),
        headers,
        b"",
    )
}

#[test]
fn storage_layout_reports_the_bucket_location_without_namespace_or_placement() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"regional-bucket","location":"asia-northeast1"}"#,
        )
        .json();

    assert_eq!(
        layout(&server, "test-bucket", "", "").json(),
        json!({
            "kind": "storage#storageLayout",
            "bucket": "test-bucket",
            "location": "US",
            "locationType": "multi-region",
        })
    );
    // gcloud 591.0.0 sends exactly this request before every copy to or from a bucket.
    let regional = layout(&server, "regional-bucket", "?alt=json", "").json();
    assert_eq!(regional["location"], "ASIA-NORTHEAST1");
    assert_eq!(regional["locationType"], "region");
    // The layout agrees with the bucket resource the same caller reads.
    let bucket = server
        .request("GET", "/storage/v1/b/regional-bucket", "", b"")
        .json();
    assert_eq!(regional["location"], bucket["location"]);
    assert_eq!(regional["locationType"], bucket["locationType"]);
}

#[test]
fn storage_layout_parameters_select_fields_and_scope_the_permission_check() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();

    // `prefix` only narrows the permission check, so the response is unchanged.
    assert_eq!(
        layout(
            &server,
            "test-bucket",
            "?prefix=logs%2F&prettyPrint=false",
            ""
        )
        .json(),
        layout(&server, "test-bucket", "", "").json()
    );
    assert_eq!(
        layout(&server, "test-bucket", "?fields=bucket,locationType", "").json(),
        json!({"bucket": "test-bucket", "locationType": "multi-region"})
    );
    layout(&server, "test-bucket", "?fields=storageClass", "").error(400, "invalidParameter");
    layout(&server, "test-bucket", "?projection=full", "").error(400, "invalidParameter");
    layout(&server, "test-bucket", "?alt=media", "").error(400, "badRequest");
    layout(&server, "test-bucket", "?userProject=billing", "").error(400, "badRequest");
}

#[test]
fn storage_layout_rejects_missing_buckets_bodies_and_other_methods() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();

    layout(&server, "missing-bucket", "", "").error(404, "notFound");
    layout(&server, "Invalid_Bucket", "", "").error(400, "invalidParameter");
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/storageLayout",
            "Content-Type: application/json\r\n",
            b"{}",
        )
        .error(400, "invalidArgument");
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let reply = server.request(method, "/storage/v1/b/test-bucket/storageLayout", "", b"");
        reply.error(405, "methodNotAllowed");
        assert_eq!(reply.headers["allow"], "GET", "{method}");
    }
}

#[test]
fn storage_layout_requires_object_list_permission_in_strict_mode() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();

    assert_eq!(
        layout(&server, "test-bucket", "", ADMIN).json()["bucket"],
        "test-bucket"
    );
    // roles/storage.objectViewer grants storage.objects.list.
    assert_eq!(
        layout(
            &server,
            "test-bucket",
            "",
            "Authorization: Bearer viewer\r\n"
        )
        .json()["bucket"],
        "test-bucket"
    );
    // roles/storage.objectCreator lacks it, as does a principal without any grant.
    for token in ["creator", "scoped"] {
        layout(
            &server,
            "test-bucket",
            "?prefix=logs%2F",
            &format!("Authorization: Bearer {token}\r\n"),
        )
        .error(403, "forbidden");
    }
    layout(&server, "test-bucket", "", "").error(401, "required");
    layout(
        &server,
        "test-bucket",
        "",
        "Authorization: Bearer unknown\r\n",
    )
    .error(401, "authError");
    // A missing bucket answers as buckets.get does for the same caller.
    let missing = layout(&server, "missing-bucket", "", ADMIN);
    let get = server.request("GET", "/storage/v1/b/missing-bucket", ADMIN, b"");
    assert_eq!(missing.status, get.status);
    let reason = |reply: &object_effects_support::Reply| {
        serde_json::from_slice::<Value>(&reply.body).unwrap()["error"]["errors"][0]["reason"]
            .clone()
    };
    assert_eq!(reason(&missing), reason(&get));
}

#[test]
fn storage_layout_follows_the_legacy_profile_location() {
    let dir = TestDir::new();
    let server = Server::start_with_args(&dir, &["--compat-profile", "legacy-emulator"]);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"test-bucket"}"#,
        )
        .json();
    let bucket = server
        .request("GET", "/storage/v1/b/test-bucket", "", b"")
        .json();
    let layout = layout(&server, "test-bucket", "", "").json();
    assert_eq!(layout["location"], bucket["location"]);
    assert_eq!(layout["locationType"], bucket["locationType"]);
}
