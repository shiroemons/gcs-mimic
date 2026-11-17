//! The Discovery resumable protocol path `POST /resumable/upload/storage/v1/b/{bucket}/o`,
//! where gcloud storage (apitools) initiates resumable uploads, and the session URI that an
//! initiation there returns on the same path.

use crate::common;
use common::TestDir;

use common::effects::{Reply, Server};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const PROTOCOL_PATH: &str = "/resumable/upload/storage/v1/b/test-bucket/o";
const UPLOAD_PATH: &str = "/upload/storage/v1/b/test-bucket/o";

/// The `Location` path of a successful initiation, relative to the advertised API base URL.
fn location_path(reply: &Reply) -> String {
    reply.expect(200);
    let location = &reply.headers["location"];
    let path = location.strip_prefix("http://127.0.0.1:4443").unwrap();
    assert!(path.contains("upload_id="), "{location}");
    path.to_owned()
}

/// The session path of a successful initiation on the protocol path, which keeps that path
/// (recorded 2026-11-17, case obj-probe-resumable-alias-session).
fn session_path(reply: &Reply) -> String {
    let path = location_path(reply);
    assert!(path.starts_with(&format!("{PROTOCOL_PATH}?")), "{path}");
    path
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

/// Recorded 2026-11-17 (case obj-probe-resumable-alias-session): Cloud Storage served a chunk
/// with an open total, a status query and the final chunk on the alias session URI as on the
/// upload session URI. POST (the Go transport's form) and the cancelling DELETE are served
/// there too, as on the upload session URI, by inference.
#[test]
fn resumable_protocol_path_serves_its_session_uri_like_the_upload_session_uri() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let path = session_path(&server.request(
        "POST",
        &format!("{PROTOCOL_PATH}?uploadType=resumable&name=alias.txt"),
        "",
        br#"{"contentType":"text/plain"}"#,
    ));
    assert!(
        path.starts_with(&format!(
            "{PROTOCOL_PATH}?name=alias.txt&uploadType=resumable&upload_id="
        )),
        "{path}"
    );
    let chunk = vec![b'a'; 256 * 1024];
    let open = server.request("PUT", &path, "Content-Range: bytes 0-262143/*\r\n", &chunk);
    let status = server.request("PUT", &path, "Content-Range: bytes */*\r\n", b"");
    for reply in [&open, &status] {
        reply.expect(308);
        assert_eq!(reply.headers["range"], "bytes=0-262143");
        assert_eq!(reply.headers["x-goog-running-hash"], "crc32c=374w4A==");
        assert_eq!(
            reply.headers["x-range-md5"],
            "c946b71bb69c07daf25470742c967e7c"
        );
    }
    let object = server
        .request(
            "PUT",
            &path,
            "Content-Range: bytes 262144-262148/262149\r\n",
            b"hello",
        )
        .json_ok();
    assert_eq!(object["name"], "alias.txt");
    assert_eq!(object["contentType"], "text/plain");
    assert_eq!(object["size"], "262149");
    assert_eq!(object["crc32c"], "ySLq0Q==");
    assert_eq!(server.get("alias.txt").json_ok()["size"], "262149");

    let posted = session_path(&server.request(
        "POST",
        &format!("{PROTOCOL_PATH}?uploadType=resumable&name=posted.txt"),
        "",
        b"",
    ));
    server
        .request("POST", &posted, "Content-Range: bytes 0-2/3\r\n", b"abc")
        .json_ok();
    assert_eq!(server.get("posted.txt?alt=media").body, b"abc");

    let initiate = |prefix: &str| {
        server.request(
            "POST",
            &format!("{prefix}?uploadType=resumable&name=cancelled.txt"),
            "",
            b"",
        )
    };
    let alias = session_path(&initiate(PROTOCOL_PATH));
    let upload = location_path(&initiate(UPLOAD_PATH));
    assert!(upload.starts_with(&format!("{UPLOAD_PATH}?")), "{upload}");
    for (method, range) in [("DELETE", ""), ("PUT", "Content-Range: bytes */*\r\n")] {
        let on_alias = server.request(method, &alias, range, b"");
        let on_upload = server.request(method, &upload, range, b"");
        assert_ne!(on_alias.status, 200, "{method}");
        assert_eq!(on_alias.status, on_upload.status, "{method}");
        assert_eq!(on_alias.body, on_upload.body, "{method}");
    }
    // An unknown session answers as on the upload session URI.
    let query = "uploadType=resumable&upload_id=abc";
    let on_alias = server.request("POST", &format!("{PROTOCOL_PATH}?{query}"), "", b"x");
    let on_upload = server.request("POST", &format!("{UPLOAD_PATH}?{query}"), "", b"x");
    assert_ne!(on_alias.status, 200);
    assert_eq!(on_alias.status, on_upload.status);
    assert_eq!(on_alias.body, on_upload.body);
    server.get("cancelled.txt").expect(404);
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
    bogus.expect(400);
    for query in [
        "uploadType=media&name=x",
        "uploadType=multipart",
        "uploadType=bogus&name=x",
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
    server.get("x").expect(404);
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
    server
        .request("PUT", PROTOCOL_PATH, "", b"xml object")
        .expect(200);
    let read = server.request("GET", PROTOCOL_PATH, "", b"");
    read.expect(200);
    assert_eq!(read.body, b"xml object");
    // An XML multipart initiation carries no uploadType, so it stays an XML request.
    let initiated = server.request("POST", &format!("{PROTOCOL_PATH}?uploads"), "", b"");
    initiated.expect(200);
    assert!(String::from_utf8_lossy(&initiated.body).contains("<UploadId>"));
    // An XML resumable session URI for that object has the alias session's path and
    // `upload_id`, but no `uploadType`, so its requests stay with the XML API too.
    let started = server.request("POST", PROTOCOL_PATH, "x-goog-resumable: start\r\n", b"");
    started.expect(201);
    let location = &started.headers["location"];
    let session = location.strip_prefix("http://127.0.0.1:4443").unwrap();
    assert!(
        session.starts_with(&format!("{PROTOCOL_PATH}?upload_id=")),
        "{location}"
    );
    server
        .request("PUT", session, "", b"xml session")
        .expect(200);
    assert_eq!(
        server.request("GET", PROTOCOL_PATH, "", b"").body,
        b"xml session"
    );
    server.request("DELETE", PROTOCOL_PATH, "", b"").expect(204);
}
