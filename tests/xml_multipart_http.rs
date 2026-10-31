mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::{Reply, Server};
use serde_json::json;

fn tag(body: &str, name: &str) -> String {
    body.split_once(&format!("<{name}>"))
        .unwrap()
        .1
        .split_once(&format!("</{name}>"))
        .unwrap()
        .0
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn bucket(server: &Server, auth: &str) {
    server
        .request(
            "POST",
            "/storage/v1/b?project=project",
            auth,
            br#"{"name":"multipart-bucket"}"#,
        )
        .status(200);
}

fn begin(server: &Server, name: &str, auth: &str) -> String {
    let reply = server.request(
        "POST",
        &format!("/multipart-bucket/{name}?uploads"),
        &format!("{auth}Content-Type: text/plain\r\nx-goog-meta-source: multipart\r\n"),
        b"",
    );
    reply.status(200);
    tag(&reply.body, "UploadId")
}

fn part(server: &Server, name: &str, id: &str, number: u16, auth: &str, bytes: &[u8]) -> String {
    let reply = server.request(
        "PUT",
        &format!("/multipart-bucket/{name}?uploadId={id}&partNumber={number}"),
        auth,
        bytes,
    );
    reply.status(200);
    reply
        .headers
        .lines()
        .find_map(|line| line.strip_prefix("etag: "))
        .unwrap()
        .into()
}

fn complete(server: &Server, name: &str, id: &str, auth: &str, parts: &[(u16, &str)]) -> Reply {
    let mut body = String::from("<CompleteMultipartUpload>");
    for (number, etag) in parts {
        body.push_str(&format!(
            "<Part><PartNumber>{number}</PartNumber><ETag>{etag}</ETag></Part>"
        ));
    }
    body.push_str("</CompleteMultipartUpload>");
    server.request(
        "POST",
        &format!("/multipart-bucket/{name}?uploadId={id}"),
        &format!("{auth}Content-Type: application/xml\r\n"),
        body.as_bytes(),
    )
}

#[test]
fn multipart_manifest_rejects_malformed_xml_without_publication_and_accepts_chunked_standard_xml() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    assert_eq!(
        server
            .request("PUT", "/multipart-bucket/value", "", b"previous")
            .status,
        200
    );
    let id = begin(&server, "value", "");
    let etag = part(&server, "value", &id, 1, "", b"completed bytes");
    let path = format!("/multipart-bucket/value?uploadId={id}");
    let part_xml = format!("<Part><PartNumber>1</PartNumber><ETag>{etag}</ETag></Part>");
    for body in [
        format!("<?xml garbage?><CompleteMultipartUpload>{part_xml}</CompleteMultipartUpload>"),
        format!("<CompleteMultipartUploadxmlns=\"http://doc.s3.amazonaws.com/2006-03-01\">{part_xml}</CompleteMultipartUploadxmlns>"),
        format!("<CompleteMultipartUpload>{part_xml}</CompleteMultipartUpload><other/>"),
        format!("<CompleteMultipartUpload>{part_xml}</CompleteMultipartUpload>trailing"),
        format!("<?xml version='1.0' version='1.0'?><CompleteMultipartUpload>{part_xml}</CompleteMultipartUpload>"),
        format!("<CompleteMultipartUpload xmlns='wrong'>{part_xml}</CompleteMultipartUpload>"),
        format!("<!DOCTYPE x [<!ENTITY secret 'value'>]><CompleteMultipartUpload>{part_xml}</CompleteMultipartUpload>"),
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>&external;</ETag></Part></CompleteMultipartUpload>".into(),
    ] {
        let reply = server.request("POST", &path, "Content-Type: application/xml\r\n", body.as_bytes());
        reply.status(400);
        assert_eq!(tag(&reply.body, "Code"), "MalformedXML");
        assert_eq!(server.request("GET", "/multipart-bucket/value", "", b"").body, "previous");
        let listed = server.request("GET", &path, "", b"");
        listed.status(200);
        assert_eq!(tag(&listed.body, "ETag"), etag);
    }
    let escaped = etag.replace('&', "&amp;").replace('"', "&quot;");
    let body = format!(
        "<?xml version = '1.0' encoding = \"UTF-8\" standalone = 'yes'?><!--before--><m:CompleteMultipartUpload xmlns:m = 'http://s3.amazonaws.com/doc/2006-03-01/'><m:Part><m:ETag>{escaped}</m:ETag><m:PartNumber>&#49;</m:PartNumber></m:Part></m:CompleteMultipartUpload><!--after-->"
    );
    let mut chunks = Vec::new();
    for byte in body.bytes() {
        chunks.extend_from_slice(&[b'1', b'\r', b'\n', byte, b'\r', b'\n']);
    }
    chunks.extend_from_slice(b"0\r\n\r\n");
    let reply = server.raw(&format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nContent-Type: application/xml\r\n\r\n"), &chunks);
    reply.status(200);
    assert_eq!(
        server
            .request("GET", "/multipart-bucket/value", "", b"")
            .body,
        "completed bytes"
    );
    server.request("GET", &path, "", b"").status(404);
}

#[test]
fn multipart_wire_round_trip_sparse_parts_paging_restart_and_checksum_metadata() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--connection-timeout-ms", "30000"]);
    bucket(&server, "");
    let id = begin(&server, "dir%2Fvalue", "");
    let first = vec![b'a'; 5 * 1024 * 1024];
    let etag = part(&server, "dir%2Fvalue", &id, 2, "", &first);
    let old = part(&server, "dir%2Fvalue", &id, 9, "", b"discarded");
    let last = part(&server, "dir%2Fvalue", &id, 9, "", b"tail");
    assert_ne!(old, last);
    let listed = server.request(
        "GET",
        &format!("/multipart-bucket/dir%2Fvalue?uploadId={id}&max-parts=1"),
        "",
        b"",
    );
    listed.status(200);
    assert_eq!(tag(&listed.body, "PartNumber"), "2");
    assert_eq!(tag(&listed.body, "NextPartNumberMarker"), "2");
    let listed = server.request(
        "GET",
        &format!("/multipart-bucket/dir%2Fvalue?uploadId={id}&part-number-marker=2"),
        "",
        b"",
    );
    listed.status(200);
    assert_eq!(tag(&listed.body, "PartNumber"), "9");
    assert_eq!(tag(&listed.body, "ETag"), last);
    let objects = server.request("GET", "/storage/v1/b/multipart-bucket/o", "", b"");
    objects.status(200);
    assert!(objects.json().get("items").is_none());
    let uploads = server.request("GET", "/multipart-bucket?uploads&delimiter=%2F", "", b"");
    uploads.status(200);
    assert_eq!(tag(&uploads.body, "Prefix"), "");
    assert!(
        uploads
            .body
            .contains("<CommonPrefixes><Prefix>dir/</Prefix>")
    );
    drop(server);
    let server = Server::start(&dir, &["--connection-timeout-ms", "30000"]);
    let reply = complete(&server, "dir%2Fvalue", &id, "", &[(2, &etag), (9, &last)]);
    reply.status(200);
    assert!(reply.headers.contains("x-goog-hash: crc32c="));
    assert!(!reply.headers.contains("md5="));
    let metadata = server.request(
        "GET",
        "/storage/v1/b/multipart-bucket/o/dir%2Fvalue",
        "",
        b"",
    );
    metadata.status(200);
    let metadata = metadata.json();
    assert!(metadata.get("md5Hash").is_none());
    assert!(metadata.get("componentCount").is_none());
    assert_eq!(metadata["metadata"]["source"], "multipart");
    let mut expected = String::from_utf8(first).unwrap();
    expected.push_str("tail");
    assert_eq!(
        server
            .request("GET", "/multipart-bucket/dir%2Fvalue", "", b"")
            .body,
        expected
    );
    complete(&server, "dir%2Fvalue", &id, "", &[(2, &etag), (9, &last)]).status(404);
    drop(server);
    let server = Server::start(&dir, &[]);
    let head = server.request("HEAD", "/multipart-bucket/dir%2Fvalue", "", b"");
    head.status(200);
    assert!(!head.headers.contains("md5="));
    assert!(!head.headers.contains("x-goog-component-count"));
    let etag = head
        .headers
        .lines()
        .find_map(|line| line.strip_prefix("etag: "))
        .unwrap();
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/multipart-bucket/o/dir%2Fvalue",
                &format!("If-None-Match: {etag}\r\n"),
                b"",
            )
            .status,
        304
    );
}

#[test]
fn multipart_rejects_bad_completion_hashes_order_small_parts_and_unsupported_conditions() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    assert_eq!(
        server
            .request("PUT", "/multipart-bucket/value", "", b"previous")
            .status,
        200
    );
    let id = begin(&server, "value", "");
    let a = part(&server, "value", &id, 1, "", b"small");
    let b = part(&server, "value", &id, 2, "", b"last");
    for (parts, code) in [
        (vec![(2, b.as_str()), (1, a.as_str())], "InvalidPartOrder"),
        (vec![(1, "\"wrong\"")], "InvalidPart"),
        (vec![(1, a.as_str()), (2, b.as_str())], "InvalidArgument"),
    ] {
        let reply = complete(&server, "value", &id, "", &parts);
        reply.status(400);
        assert_eq!(tag(&reply.body, "Code"), code);
        assert_eq!(
            server
                .request("GET", "/multipart-bucket/value", "", b"")
                .body,
            "previous"
        );
    }
    let bad = server.request(
        "PUT",
        &format!("/multipart-bucket/value?uploadId={id}&partNumber=1"),
        "Content-MD5: AAAAAAAAAAAAAAAAAAAAAA==\r\n",
        b"bad",
    );
    bad.status(400);
    assert_eq!(tag(&bad.body, "Code"), "BadDigest");
    server
        .request(
            "PUT",
            &format!("/multipart-bucket/value?uploadId={id}&partNumber=1"),
            "Content-Encoding: gzip\r\n",
            &ingress_support::gzip(b"unsupported part encoding"),
        )
        .status(400);
    server
        .request(
            "POST",
            "/multipart-bucket/value?uploads",
            "x-goog-if-generation-match: 0\r\n",
            b"",
        )
        .status(400);
    server
        .request("POST", "/multipart-bucket/bad%01name?uploads", "", b"")
        .status(400);
    server
        .request("GET", "/multipart-bucket?uploads&prefix=%01", "", b"")
        .status(400);
    server
        .request(
            "PUT",
            &format!("/multipart-bucket/value?uploadId={id}&partNumber=0"),
            "",
            b"bad",
        )
        .status(400);
    let aborted = server.request(
        "DELETE",
        &format!("/multipart-bucket/value?uploadId={id}"),
        "",
        b"",
    );
    assert_eq!(aborted.status, 204);
    let reply = server.request(
        "GET",
        &format!("/multipart-bucket/value?uploadId={id}"),
        "",
        b"",
    );
    reply.status(404);
    assert_eq!(tag(&reply.body, "Code"), "NoSuchUpload");
    assert_eq!(
        server
            .request("GET", "/multipart-bucket/value", "", b"")
            .body,
        "previous"
    );
}

/// Bucket deletion and restart recovery sweep multipart sessions in the background; until the
/// sweep releases the lock, multipart requests answer the retryable 503 Busy.
fn after_sweep(mut send: impl FnMut() -> Reply) -> Reply {
    for _ in 0..100 {
        let reply = send();
        if reply.status != 503 || !reply.body.contains("already in progress") {
            return reply;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("multipart sweep did not become idle");
}

#[test]
fn multipart_reset_and_bucket_recreation_invalidate_sessions_and_remove_parts() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    bucket(&server, "");
    let id = begin(&server, "value", "");
    part(&server, "value", &id, 1, "", b"unfinished");
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/multipart-bucket", "", b"")
            .status,
        204
    );
    bucket(&server, "");
    after_sweep(|| {
        server.request(
            "PUT",
            &format!("/multipart-bucket/value?uploadId={id}&partNumber=1"),
            "",
            b"retarget",
        )
    })
    .status(404);
    let id = begin(&server, "value", "");
    part(&server, "value", &id, 1, "", b"discard");
    let mut reset = None;
    for _ in 0..100 {
        let reply = server.request(
            "POST",
            "/_internal/reset",
            "Content-Type: application/json\r\n",
            br#"{"confirm":"gcs-mimic"}"#,
        );
        if reply.status != 503 {
            reset = Some(reply);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    reset
        .expect("maintenance eventually becomes idle")
        .status(200);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    drop(server);
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    after_sweep(|| {
        server.request(
            "GET",
            &format!("/multipart-bucket/value?uploadId={id}"),
            "",
            b"",
        )
    })
    .status(404);
}

#[test]
fn multipart_strict_reauthorizes_each_request_and_preserves_creator_overwrite_boundary() {
    let dir = TestDir::new();
    let registry = dir.0.join("identities.json");
    std::fs::write(&registry,json!({"identities":[{"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},{"id":"creator@example.invalid","tokens":["creator"],"project_roles":{"project":["roles/storage.objectCreator"]}},{"id":"reader@example.invalid","tokens":["reader"],"project_roles":{"project":["roles/storage.objectViewer"]}}]}).to_string()).unwrap();
    let server = Server::start(
        &dir,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    let admin = "Authorization: Bearer admin\r\n";
    let creator = "Authorization: Bearer creator\r\n";
    let reader = "Authorization: Bearer reader\r\n";
    bucket(&server, admin);
    let id = begin(&server, "value", creator);
    server
        .request(
            "PUT",
            &format!("/multipart-bucket/value?uploadId={id}&partNumber=1"),
            reader,
            b"denied",
        )
        .status(403);
    let etag = part(&server, "value", &id, 1, creator, b"allowed");
    server
        .request(
            "GET",
            &format!("/multipart-bucket/value?uploadId={id}"),
            reader,
            b"",
        )
        .status(403);
    server
        .request("GET", "/multipart-bucket?uploads", creator, b"")
        .status(403);
    complete(&server, "value", &id, creator, &[(1, &etag)]).status(200);
    let id = begin(&server, "value", creator);
    let etag = part(&server, "value", &id, 1, creator, b"overwrite");
    complete(&server, "value", &id, creator, &[(1, &etag)]).status(403);
    assert_eq!(
        server
            .request("GET", "/multipart-bucket/value", admin, b"")
            .body,
        "allowed"
    );
    assert_eq!(
        server
            .request(
                "DELETE",
                &format!("/multipart-bucket/value?uploadId={id}"),
                creator,
                b"",
            )
            .status,
        204
    );
    server
        .request("POST", "/multipart-bucket/anonymous?uploads", "", b"")
        .status(401);
}

#[test]
fn multipart_upload_listing_resumes_duplicate_keys_and_skips_delimiter_groups() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    let one = begin(&server, "same", "");
    let two = begin(&server, "same", "");
    let first = server.request(
        "GET",
        "/multipart-bucket?uploads&prefix=same&max-uploads=1",
        "",
        b"",
    );
    first.status(200);
    assert_eq!(tag(&first.body, "IsTruncated"), "true");
    let id = tag(&first.body, "NextUploadIdMarker");
    assert!(id == one || id == two);
    let next=server.request("GET",&format!("/multipart-bucket?uploads&prefix=same&max-uploads=1&key-marker=same&upload-id-marker={id}"),"",b"");
    next.status(200);
    assert_ne!(tag(&next.body, "UploadId"), id);
    assert_eq!(tag(&next.body, "IsTruncated"), "false");
    for name in ["folder/a", "folder/b", "z"] {
        begin(&server, name, "");
    }
    let first = server.request(
        "GET",
        "/multipart-bucket?uploads&delimiter=%2F&max-uploads=1",
        "",
        b"",
    );
    first.status(200);
    assert!(
        first
            .body
            .contains("<CommonPrefixes><Prefix>folder/</Prefix>")
    );
    assert_eq!(tag(&first.body, "NextKeyMarker"), "folder/");
    let next=server.request("GET","/multipart-bucket?uploads&delimiter=%2F&max-uploads=1&key-marker=folder%2F&upload-id-marker=","",b"");
    next.status(200);
    assert!(!next.body.contains("<CommonPrefixes>"));
    assert_eq!(tag(&next.body, "Key"), "same");
}

#[test]
fn multipart_customer_key_must_match_initiation_for_parts_completion_and_reads() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    let key = "x-goog-encryption-algorithm: AES256\r\nx-goog-encryption-key: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\r\nx-goog-encryption-key-sha256: Zmh6rfhivXdsj8GLjp+OIAiXFIVu4jOzkCpZHQ1fKSU=\r\n";
    let id = begin(&server, "encrypted", key);
    let denied = server.request(
        "PUT",
        &format!("/multipart-bucket/encrypted?uploadId={id}&partNumber=1"),
        "",
        b"secret",
    );
    denied.status(400);
    assert_eq!(
        tag(&denied.body, "Code"),
        "ResourceIsEncryptedWithCustomerEncryptionKey"
    );
    let etag = part(&server, "encrypted", &id, 1, key, b"secret");
    complete(&server, "encrypted", &id, "", &[(1, &etag)]).status(400);
    let complete = complete(&server, "encrypted", &id, key, &[(1, &etag)]);
    complete.status(200);
    assert!(
        complete
            .headers
            .contains("x-goog-encryption-algorithm: AES256")
    );
    assert!(complete.headers.contains("crc32c="));
    assert!(!complete.headers.contains("md5="));
    server
        .request("GET", "/multipart-bucket/encrypted", "", b"")
        .status(400);
    assert_eq!(
        server
            .request("GET", "/multipart-bucket/encrypted", key, b"")
            .body,
        "secret"
    );
}

#[test]
fn multipart_completion_rechecks_destination_hold_without_discarding_parts() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    assert_eq!(
        server
            .request("PUT", "/multipart-bucket/held", "", b"previous")
            .status,
        200
    );
    server
        .request(
            "PATCH",
            "/storage/v1/b/multipart-bucket/o/held",
            "Content-Type: application/json\r\n",
            br#"{"temporaryHold":true}"#,
        )
        .status(200);
    let id = begin(&server, "held", "");
    let etag = part(&server, "held", &id, 1, "", b"new");
    complete(&server, "held", &id, "", &[(1, &etag)]).status(403);
    assert_eq!(
        server
            .request("GET", "/multipart-bucket/held", "", b"")
            .body,
        "previous"
    );
    server
        .request(
            "PATCH",
            "/storage/v1/b/multipart-bucket/o/held",
            "Content-Type: application/json\r\n",
            br#"{"temporaryHold":false}"#,
        )
        .status(200);
    complete(&server, "held", &id, "", &[(1, &etag)]).status(200);
    assert_eq!(
        server
            .request("GET", "/multipart-bucket/held", "", b"")
            .body,
        "new"
    );
}
