mod common;
use common::effects as object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::json;

#[test]
fn ruby_default_create_bucket() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    for (index, autoclass) in [
        json!(null),
        json!({}),
        json!({"enabled": false}),
        json!({"enabled": null}),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("ruby-default-{index}");
        let created = server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                "",
                json!({"name": name, "autoclass": autoclass})
                    .to_string()
                    .as_bytes(),
            )
            .json_ok();
        assert_eq!(created["name"], name);
        assert!(created.get("autoclass").is_none());
        let fetched = server
            .request("GET", &format!("/storage/v1/b/{name}"), "", b"")
            .json_ok();
        assert!(fetched.get("autoclass").is_none());
    }
    // Enabled Autoclass is covered by bucket_autoclass_http; malformed settings still fail.
    for autoclass in [
        json!({"enabled": false, "terminalStorageClass": "ARCHIVE"}),
        json!({"enabled": "false"}),
        json!({"unknown": false}),
        json!(false),
    ] {
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                "",
                json!({"name": "unsupported-autoclass", "autoclass": autoclass})
                    .to_string()
                    .as_bytes(),
            )
            .json_reason(400, "invalid");
        server
            .request("GET", "/storage/v1/b/unsupported-autoclass", "", b"")
            .expect(404);
    }
}

#[test]
fn ruby_default_create_file_content_type() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    for (name, header) in [("absent", ""), ("empty", "X-Upload-Content-Type: \r\n")] {
        let initiated = server.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}"),
            header,
            b"{}",
        );
        initiated.expect(200);
        let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
        let object = server
            .request(
                "PUT",
                uri.path_and_query().unwrap().as_str(),
                "Content-Range: bytes 0-2/3\r\n",
                b"abc",
            )
            .json_ok();
        assert_eq!(object["contentType"], "application/octet-stream");
        assert_eq!(
            server.get(name).json_ok()["contentType"],
            object["contentType"]
        );
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!("/storage/v1/b/test-bucket/o/{name}?alt=media"),
                    "",
                    b""
                )
                .body,
            b"abc"
        );
    }
    let invalid = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=invalid",
        "X-Upload-Content-Type: invalid\r\n",
        b"{}",
    );
    invalid.expect(400);
    server.get("invalid").expect(404);
}

#[test]
fn ruby_default_compose() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("source", b"abc").json_ok();
    let path = "/storage/v1/b/test-bucket/o/composed/compose";
    let result = server.request("POST", path, "Authorization: Bearer admin\r\n",
        br#"{"sourceObjects":[{"name":"source"},{"name":"source"}],"deleteSourceObjects":null,"destination":null}"#).json_ok();
    assert_eq!(result["size"], "6");
    assert!(result.get("contentType").is_none());
    let bytes = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/composed?alt=media",
        "Authorization: Bearer admin\r\n",
        b"",
    );
    assert_eq!(bytes.body, b"abcabc");
    assert_eq!(bytes.headers["content-type"], "application/octet-stream");
    assert_eq!(server.get("source").json_ok()["size"], "3");
    server
        .request(
            "POST",
            path,
            "Authorization: Bearer admin\r\n",
            br#"{"sourceObjects":[{"name":"source"}],"deleteSourceObjects":true}"#,
        )
        .json_ok();
    server.get("source").expect(404);
    server.request("POST", path, "Authorization: Bearer admin\r\n",
        br#"{"sourceObjects":[{"name":"composed"}],"destination":{"acl":[]},"deleteSourceObjects":null}"#).json_reason(400, "badRequest");
    assert_eq!(server.get("composed").json_ok()["size"], "3");
}
