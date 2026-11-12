//! `buckets.getStorageLayout`: the resource, its parameters and the `storage.objects.list`
//! permission the reference names for it.

use crate::common;
use common::TestDir;

use common::effects::Server;
use serde_json::{Value, json};

const ADMIN: &str = "Authorization: Bearer admin\r\n";

fn layout(server: &Server, bucket: &str, query: &str, headers: &str) -> common::effects::Reply {
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
        .json_ok();

    assert_eq!(
        layout(&server, "test-bucket", "", "").json_ok(),
        json!({
            "kind": "storage#storageLayout",
            "bucket": "test-bucket",
            "location": "US",
            "locationType": "multi-region",
        })
    );
    // gcloud 591.0.0 sends exactly this request before every copy to or from a bucket.
    let regional = layout(&server, "regional-bucket", "?alt=json", "").json_ok();
    assert_eq!(regional["location"], "ASIA-NORTHEAST1");
    assert_eq!(regional["locationType"], "region");
    // The layout agrees with the bucket resource the same caller reads.
    let bucket = server
        .request("GET", "/storage/v1/b/regional-bucket", "", b"")
        .json_ok();
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
        .json_ok(),
        layout(&server, "test-bucket", "", "").json_ok()
    );
    assert_eq!(
        layout(&server, "test-bucket", "?fields=bucket,locationType", "").json_ok(),
        json!({"bucket": "test-bucket", "locationType": "multi-region"})
    );
    layout(&server, "test-bucket", "?fields=storageClass", "").json_reason(400, "invalidParameter");
    layout(&server, "test-bucket", "?projection=full", "").json_reason(400, "invalidParameter");
    layout(&server, "test-bucket", "?alt=media", "").json_reason(400, "badRequest");
    layout(&server, "test-bucket", "?userProject=billing", "").json_reason(400, "badRequest");
}

#[test]
fn storage_layout_rejects_missing_buckets_bodies_and_other_methods() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();

    layout(&server, "missing-bucket", "", "").json_reason(404, "notFound");
    layout(&server, "Invalid_Bucket", "", "").json_reason(400, "invalidParameter");
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/storageLayout",
            "Content-Type: application/json\r\n",
            b"{}",
        )
        .json_reason(400, "invalidArgument");
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let reply = server.request(method, "/storage/v1/b/test-bucket/storageLayout", "", b"");
        reply.json_reason(405, "methodNotAllowed");
        assert_eq!(reply.headers["allow"], "GET", "{method}");
    }
}

#[test]
fn storage_layout_requires_object_list_permission_in_strict_mode() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();

    assert_eq!(
        layout(&server, "test-bucket", "", ADMIN).json_ok()["bucket"],
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
        .json_ok()["bucket"],
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
        .json_reason(403, "forbidden");
    }
    layout(&server, "test-bucket", "", "").json_reason(401, "required");
    layout(
        &server,
        "test-bucket",
        "",
        "Authorization: Bearer unknown\r\n",
    )
    .json_reason(401, "authError");
    // A missing bucket answers as buckets.get does for the same caller.
    let missing = layout(&server, "missing-bucket", "", ADMIN);
    let get = server.request("GET", "/storage/v1/b/missing-bucket", ADMIN, b"");
    assert_eq!(missing.status, get.status);
    let reason = |reply: &common::effects::Reply| {
        serde_json::from_slice::<Value>(&reply.body).unwrap()["error"]["errors"][0]["reason"]
            .clone()
    };
    assert_eq!(reason(&missing), reason(&get));
}

#[test]
fn storage_layout_follows_the_legacy_profile_location() {
    let dir = TestDir::new();
    let server = Server::start_with_args(&dir, &["--compat-profile", "fake-gcs-server"]);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"test-bucket"}"#,
        )
        .json_ok();
    let bucket = server
        .request("GET", "/storage/v1/b/test-bucket", "", b"")
        .json_ok();
    let layout = layout(&server, "test-bucket", "", "").json_ok();
    assert_eq!(layout["location"], bucket["location"]);
    assert_eq!(layout["locationType"], bucket["locationType"]);
}
