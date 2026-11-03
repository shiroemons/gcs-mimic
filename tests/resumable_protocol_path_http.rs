//! The Discovery resumable protocol path `POST /resumable/upload/storage/v1/b/{bucket}/o`,
//! where gcloud storage (apitools) initiates resumable uploads.

mod common;
use common::effects as object_effects_support;

use object_effects_support::{Reply, Server, common::TestDir};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const PROTOCOL_PATH: &str = "/resumable/upload/storage/v1/b/test-bucket/o";

/// The session path of a successful initiation, relative to the advertised API base URL.
fn session_path(reply: &Reply) -> String {
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let location = &reply.headers["location"];
    let path = location.strip_prefix("http://127.0.0.1:4443").unwrap();
    assert!(
        path.starts_with("/upload/storage/v1/b/test-bucket/o?") && path.contains("upload_id="),
        "{location}"
    );
    path.to_owned()
}

#[test]
fn resumable_protocol_path_initiates_the_same_session_as_the_upload_path() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    // gcloud 591.0.0 sends exactly this initiation for a stdin stream or a file of 8 MiB or more.
    let reply = server.request(
        "POST",
        &format!("{PROTOCOL_PATH}?alt=json&uploadType=resumable"),
        "X-Upload-Content-Type: text/plain\r\nX-Upload-Content-Length: 5\r\n",
        br#"{"bucket": "test-bucket", "contentType": "text/plain", "name": "streamed.txt"}"#,
    );
    let path = session_path(&reply);
    let object = server
        .request("PUT", &path, "Content-Range: bytes 0-4/5\r\n", b"hello")
        .json_ok();
    assert_eq!(object["name"], "streamed.txt");
    assert_eq!(object["contentType"], "text/plain");
    assert_eq!(server.get("streamed.txt?alt=media").body, b"hello");
    // The query name and an empty body work as on /upload/storage/v1.
    let reply = server.request(
        "POST",
        &format!("{PROTOCOL_PATH}?uploadType=resumable&name=named.bin"),
        "",
        b"",
    );
    let path = session_path(&reply);
    server
        .request("PUT", &path, "Content-Range: bytes 0-2/3\r\n", b"abc")
        .json_ok();
    assert_eq!(server.get("named.bin?alt=media").body, b"abc");
    // Invalid names, malformed metadata and missing buckets answer as on the upload path.
    for (bucket, query, body) in [
        ("test-bucket", "uploadType=resumable", b"{}".as_slice()),
        ("test-bucket", "uploadType=resumable&name=", b"".as_slice()),
        (
            "test-bucket",
            "uploadType=resumable&name=x",
            b"{".as_slice(),
        ),
        (
            "missing-bucket",
            "uploadType=resumable&name=x",
            b"".as_slice(),
        ),
    ] {
        let upload = server.request(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?{query}"),
            "",
            body,
        );
        let protocol = server.request(
            "POST",
            &format!("/resumable/upload/storage/v1/b/{bucket}/o?{query}"),
            "",
            body,
        );
        assert_ne!(upload.status, 200, "{bucket} {query}");
        assert_eq!(protocol.status, upload.status, "{bucket} {query}");
        assert_eq!(protocol.body, upload.body, "{bucket} {query}");
    }
}

#[test]
fn resumable_protocol_path_rejects_other_upload_types_like_the_upload_path() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let bogus = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=bogus&name=x",
        "",
        b"x",
    );
    assert_eq!(bogus.status, 400);
    for query in [
        "uploadType=media&name=x",
        "uploadType=multipart",
        "uploadType=bogus&name=x",
        "uploadType=resumable&upload_id=abc",
    ] {
        let reply = server.request("POST", &format!("{PROTOCOL_PATH}?{query}"), "", b"x");
        assert_eq!(reply.status, bogus.status, "{query}");
        assert_eq!(reply.body, bogus.body, "{query}");
        assert_eq!(reply.headers["content-type"], bogus.headers["content-type"]);
    }
    // The rejection carries the JSON API CORS fields, as on the upload path, and is not read
    // through a method override or an upload Content-Encoding.
    let origin = "Origin: https://app.example\r\n";
    let bogus = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=bogus&name=x",
        origin,
        b"x",
    );
    assert_eq!(
        bogus.headers["access-control-allow-origin"],
        "https://app.example"
    );
    for headers in [
        origin.to_owned(),
        format!("{origin}X-HTTP-Method-Override: GET\r\n"),
        format!("{origin}Content-Encoding: br\r\n"),
    ] {
        let reply = server.request(
            "POST",
            &format!("{PROTOCOL_PATH}?uploadType=multipart"),
            &headers,
            b"x",
        );
        assert_eq!(reply.status, bogus.status, "{headers}");
        assert_eq!(reply.body, bogus.body, "{headers}");
        for field in [
            "access-control-allow-origin",
            "access-control-expose-headers",
        ] {
            assert_eq!(
                reply.headers.get(field),
                bogus.headers.get(field),
                "{field} {headers}"
            );
        }
    }
    assert_eq!(server.get("x").status, 404);
}

#[test]
fn resumable_protocol_path_keeps_strict_authorization() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    let initiate = |headers: &str, query: &str| {
        server.request(
            "POST",
            &format!("{PROTOCOL_PATH}?{query}"),
            headers,
            br#"{"name":"strict.txt"}"#,
        )
    };
    initiate("", "uploadType=resumable").json_reason(401, "required");
    initiate("Authorization: Bearer viewer\r\n", "uploadType=resumable")
        .json_reason(403, "forbidden");
    // The upload-type rejection orders authentication as the upload path does.
    for headers in ["", "Authorization: Bearer unknown\r\n", ADMIN] {
        let upload = server.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=bogus&name=x",
            headers,
            b"x",
        );
        let protocol = initiate(headers, "uploadType=media&name=x");
        assert_eq!(protocol.status, upload.status, "{headers}");
        assert_eq!(protocol.body, upload.body, "{headers}");
    }
    initiate(
        "Authorization: Bearer unknown\r\n",
        "uploadType=media&name=x",
    )
    .json_reason(401, "authError");
    let path = session_path(&initiate(ADMIN, "uploadType=resumable"));
    server
        .request(
            "PUT",
            &path,
            &format!("{ADMIN}Content-Range: bytes 0-1/2\r\n"),
            b"ok",
        )
        .json_ok();
}

#[test]
fn resumable_protocol_path_leaves_a_bucket_named_resumable_to_the_xml_api() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"resumable"}"#,
        )
        .json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"test-bucket"}"#,
        )
        .json_ok();
    // Path-style XML reads and writes of that object name still reach the XML API.
    assert_eq!(
        server
            .request("PUT", PROTOCOL_PATH, "", b"xml object")
            .status,
        200
    );
    let read = server.request("GET", PROTOCOL_PATH, "", b"");
    assert_eq!(read.status, 200);
    assert_eq!(read.body, b"xml object");
    // An XML multipart initiation carries no uploadType, so it stays an XML request.
    let initiated = server.request("POST", &format!("{PROTOCOL_PATH}?uploads"), "", b"");
    assert_eq!(
        initiated.status,
        200,
        "{}",
        String::from_utf8_lossy(&initiated.body)
    );
    assert!(String::from_utf8_lossy(&initiated.body).contains("<UploadId>"));
    assert_eq!(server.request("DELETE", PROTOCOL_PATH, "", b"").status, 204);
}
