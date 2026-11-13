//! Entity-tag headers on collection responses, which have no entity tag to compare. Cloud Storage
//! listed the objects with `If-None-Match: *` and with a bogus `If-Match` (case
//! obj-collection-validators).

use crate::common;
use common::TestDir;
use common::effects::Server;

const AUTH: &str = "Authorization: Bearer admin\r\n";

#[test]
fn collection_listings_ignore_entity_tag_headers() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("listed", b"hello").json_ok();
    for path in [
        "/storage/v1/b/test-bucket/o?prefix=listed",
        "/storage/v1/b?project=local",
        "/storage/v1/b/test-bucket/o/listed/acl",
    ] {
        let plain = server.request("GET", path, AUTH, b"").json_ok();
        for condition in ["If-None-Match: *\r\n", "If-Match: \"bogus\"\r\n"] {
            let reply = server
                .request("GET", path, &format!("{AUTH}{condition}"), b"")
                .json_ok();
            assert_eq!(reply, plain, "{path} {condition}");
        }
    }
    // A method without an entity tag that is not a listing keeps rejecting the headers.
    server
        .request(
            "DELETE",
            "/storage/v1/b/test-bucket/o/listed",
            &format!("{AUTH}If-Match: \"bogus\"\r\n"),
            b"",
        )
        .json_reason(400, "badRequest");
    server.get("listed").json_ok();
}
