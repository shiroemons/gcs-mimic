mod object_effects_support;

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
            .json();
        assert_eq!(created["name"], name);
        assert!(created.get("autoclass").is_none());
        let fetched = server
            .request("GET", &format!("/storage/v1/b/{name}"), "", b"")
            .json();
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
            .error(400, "invalidArgument");
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/unsupported-autoclass", "", b"")
                .status,
            404
        );
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
        assert_eq!(initiated.status, 200, "{:?}", initiated.body);
        let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
        let object = server
            .request(
                "PUT",
                uri.path_and_query().unwrap().as_str(),
                "Content-Range: bytes 0-2/3\r\n",
                b"abc",
            )
            .json();
        assert_eq!(object["contentType"], "application/octet-stream");
        assert_eq!(
            server.get(name).json()["contentType"],
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
    assert_eq!(invalid.status, 400);
    assert_eq!(server.get("invalid").status, 404);
}

#[test]
fn ruby_default_compose() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    server.put("source", b"abc").json();
    let path = "/storage/v1/b/test-bucket/o/composed/compose";
    let result = server.request("POST", path, "Authorization: Bearer admin\r\n",
        br#"{"sourceObjects":[{"name":"source"},{"name":"source"}],"deleteSourceObjects":null,"destination":null}"#).json();
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
    assert_eq!(server.get("source").json()["size"], "3");
    server
        .request(
            "POST",
            path,
            "Authorization: Bearer admin\r\n",
            br#"{"sourceObjects":[{"name":"source"}],"deleteSourceObjects":true}"#,
        )
        .json();
    assert_eq!(server.get("source").status, 404);
    server.request("POST", path, "Authorization: Bearer admin\r\n",
        br#"{"sourceObjects":[{"name":"composed"}],"destination":{"acl":[]},"deleteSourceObjects":null}"#).error(400, "badRequest");
    assert_eq!(server.get("composed").json()["size"], "3");
}
