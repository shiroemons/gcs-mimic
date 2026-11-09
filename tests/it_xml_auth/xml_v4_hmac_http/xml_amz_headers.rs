//! `x-amz-*` extension headers, which mean what they say only under an AWS signature.

use super::*;
use serde_json::Value;

const SIGNER_OWNER: (&str, &str) = ("user-signer@example.invalid", "OWNER");
const OWNERS_READER: (&str, &str) = ("project-owners-0", "READER");
const TIME: &str = "Thu, 22 Oct 2026 00:00:00 GMT";

fn fixture() -> (TestDir, Server, Key) {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    (dir, server, key)
}

fn object(server: &Server, name: &str) -> Value {
    server
        .request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o/{name}?projection=full"),
            ADMIN,
            b"",
        )
        .json_ok()
}

fn absent(server: &Server, name: &str) {
    server
        .request(
            "GET",
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            ADMIN,
            b"",
        )
        .expect(404);
}

fn acl(object: &Value) -> Vec<(String, String)> {
    let mut entries: Vec<_> = object["acl"]
        .as_array()
        .unwrap_or_else(|| panic!("no acl in {object}"))
        .iter()
        .map(|entry| {
            (
                entry["entity"].as_str().unwrap().to_owned(),
                entry["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    entries.sort_unstable();
    entries
}

fn entries(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut entries: Vec<_> = expected
        .iter()
        .map(|(entity, role)| ((*entity).to_owned(), (*role).to_owned()))
        .collect();
    entries.sort_unstable();
    entries
}

fn aws4_put(server: &Server, key: &Key, name: &str, extras: &[(&str, &str)]) -> Reply {
    let path = format!("/test-bucket/{name}");
    let signed = sign(key, "PUT", &path, true, true, extras, SystemTime::now());
    server.signed("PUT", &signed, b"")
}

#[track_caller]
fn assert_details(reply: &Reply, details: &str) {
    reply.expect(400);
    let body = String::from_utf8_lossy(&reply.body);
    let expected = format!(
        "<Code>InvalidArgument</Code><Message>Invalid argument.</Message><Details>{details}</Details>"
    );
    assert!(body.contains(&expected), "{body}");
}

#[test]
fn aws4_header_and_query_signatures_apply_amz_metadata_class_and_acl() {
    let (_dir, server, key) = fixture();
    let extras = [
        ("x-amz-meta-origin", "s3"),
        ("x-amz-storage-class", "NEARLINE"),
        ("x-amz-acl", "bucket-owner-read"),
    ];
    for (authorization_header, name) in [(true, "by-header"), (false, "by-query")] {
        let path = format!("/test-bucket/{name}");
        let signed = sign(
            &key,
            "PUT",
            &path,
            true,
            authorization_header,
            &extras,
            SystemTime::now(),
        );
        server.signed("PUT", &signed, b"payload").expect(200);
        let stored = object(&server, name);
        assert_eq!(stored["metadata"], json!({"origin":"s3"}), "{name}");
        assert_eq!(stored["storageClass"], "NEARLINE", "{name}");
        assert_eq!(
            acl(&stored),
            entries(&[SIGNER_OWNER, OWNERS_READER]),
            "{name}"
        );
        let head = server.request("HEAD", &path, ADMIN, b"");
        head.expect(200);
        assert_eq!(head.headers["x-goog-meta-origin"], "s3", "{name}");
    }
}

#[test]
fn aws4_chunked_upload_stores_amz_metadata() {
    let (_dir, server, key) = fixture();
    let bytes = b"hello world";
    let length = bytes.len().to_string();
    let signed = sign(
        &key,
        "PUT",
        "/test-bucket/chunked",
        true,
        true,
        &[
            ("x-amz-content-sha256", xml_v4_streaming::MARKER),
            ("x-amz-decoded-content-length", length.as_str()),
            ("content-encoding", "aws-chunked"),
            ("x-amz-meta-origin", "s3"),
        ],
        SystemTime::now(),
    );
    let body = xml_v4_streaming::chain(&key, &signed, &[bytes]);
    xml_v4_streaming::send(&server, &signed, &body, None, false).expect(200);
    assert_eq!(
        object(&server, "chunked")["metadata"],
        json!({"origin":"s3"})
    );
}

#[test]
fn aws4_copy_maps_the_source_directive_and_source_conditions() {
    let (_dir, server, key) = fixture();
    server
        .request(
            "PUT",
            "/test-bucket/source",
            &format!("{ADMIN}x-goog-meta-a: src\r\n"),
            b"payload",
        )
        .expect(200);
    let source = ("x-amz-copy-source", "/test-bucket/source");

    aws4_put(&server, &key, "inherited", &[source]).expect(200);
    assert_eq!(object(&server, "inherited")["metadata"], json!({"a":"src"}));

    aws4_put(
        &server,
        &key,
        "replaced",
        &[
            source,
            ("x-amz-metadata-directive", "REPLACE"),
            ("x-amz-meta-b", "new"),
            ("x-amz-acl", "bucket-owner-read"),
        ],
    )
    .expect(200);
    let replaced = object(&server, "replaced");
    assert_eq!(replaced["metadata"], json!({"b":"new"}));
    assert_eq!(acl(&replaced), entries(&[SIGNER_OWNER, OWNERS_READER]));

    let failed = aws4_put(
        &server,
        &key,
        "conditioned",
        &[source, ("x-amz-copy-source-if-match", "\"bogus\"")],
    );
    error(&failed, 400, "InvalidArgument");
    absent(&server, "conditioned");

    // The mapped name reaches the copy adapter, which does not implement date validators.
    let unsupported = aws4_put(
        &server,
        &key,
        "dated",
        &[source, ("x-amz-copy-source-if-modified-since", TIME)],
    );
    error(&unsupported, 400, "InvalidArgument");
    absent(&server, "dated");
}

#[test]
fn aws4_amz_acl_applies_at_multipart_and_resumable_initiation() {
    let (_dir, server, key) = fixture();
    let acl_header = ("x-amz-acl", "bucket-owner-read");

    let begin = sign(
        &key,
        "POST",
        "/test-bucket/multipart?uploads",
        true,
        true,
        &[acl_header],
        SystemTime::now(),
    );
    let begun = server.signed("POST", &begin, b"");
    begun.expect(200);
    let body = String::from_utf8_lossy(&begun.body).into_owned();
    let id = body
        .split_once("<UploadId>")
        .unwrap()
        .1
        .split_once("</UploadId>")
        .unwrap()
        .0;
    let path = format!("/test-bucket/multipart?uploadId={id}");
    let part_path = format!("{path}&partNumber=1");
    let signed_part = sign(&key, "PUT", &part_path, true, true, &[], SystemTime::now());
    let part = server.signed("PUT", &signed_part, b"bytes");
    part.expect(200);
    let manifest = format!(
        "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{}</ETag></Part></CompleteMultipartUpload>",
        part.headers["etag"]
    );
    // Completion publishes the object, so its caller becomes the owner.
    let complete = sign(&key, "POST", &path, true, true, &[], SystemTime::now());
    server
        .signed("POST", &complete, manifest.as_bytes())
        .expect(200);
    assert_eq!(
        acl(&object(&server, "multipart")),
        entries(&[SIGNER_OWNER, OWNERS_READER])
    );

    let start = sign(
        &key,
        "POST",
        "/test-bucket/resumable",
        true,
        true,
        &[acl_header, ("x-goog-resumable", "start")],
        SystemTime::now(),
    );
    let started = server.signed("POST", &start, b"");
    started.expect(201);
    let uri: hyper::Uri = started.headers["location"].parse().unwrap();
    server
        .request(
            "PUT",
            uri.path_and_query().unwrap().as_str(),
            "Content-Range: bytes 0-4/5\r\n",
            b"bytes",
        )
        .expect(200);
    assert_eq!(
        acl(&object(&server, "resumable")),
        entries(&[SIGNER_OWNER, OWNERS_READER])
    );
}

#[test]
fn requests_without_an_aws_signature_ignore_amz_metadata_acl_and_class() {
    let (_dir, server, key) = fixture();
    let put = |name: &str, headers: &str| {
        server.request(
            "PUT",
            &format!("/test-bucket/{name}"),
            &format!("{ADMIN}{headers}"),
            b"payload",
        )
    };

    put("metadata", "x-amz-meta-origin: s3\r\n").expect(200);
    assert_eq!(object(&server, "metadata").get("metadata"), None);

    put(
        "both",
        "x-amz-meta-origin: s3\r\nx-goog-meta-origin: goog\r\n",
    )
    .expect(200);
    assert_eq!(
        object(&server, "both")["metadata"],
        json!({"origin":"goog"})
    );

    put("class", "x-amz-storage-class: NEARLINE\r\n").expect(200);
    assert_eq!(object(&server, "class")["storageClass"], "STANDARD");

    put("directive", "x-amz-metadata-directive: REPLACE\r\n").expect(200);

    put("baseline", "").expect(200);
    put("acl", "x-amz-acl: public-read\r\n").expect(200);
    assert_eq!(
        acl(&object(&server, "acl")),
        acl(&object(&server, "baseline"))
    );

    let signed = sign(
        &key,
        "PUT",
        "/test-bucket/goog4",
        false,
        true,
        &[("x-amz-meta-origin", "s3"), ("x-amz-acl", "public-read")],
        SystemTime::now(),
    );
    server.signed("PUT", &signed, b"payload").expect(200);
    assert_eq!(object(&server, "goog4").get("metadata"), None);
}

#[test]
fn copy_source_without_an_aws_signature_is_refused_with_the_recorded_details() {
    let (_dir, server, key) = fixture();
    server
        .request("PUT", "/test-bucket/source", ADMIN, b"payload")
        .expect(200);
    let oauth = server.request(
        "PUT",
        "/test-bucket/oauth",
        &format!("{ADMIN}x-amz-copy-source: /test-bucket/source\r\n"),
        b"",
    );
    assert_details(&oauth, "Missing copy source");
    absent(&server, "oauth");

    let signed = sign(
        &key,
        "PUT",
        "/test-bucket/goog4",
        false,
        true,
        &[("x-amz-copy-source", "/test-bucket/source")],
        SystemTime::now(),
    );
    assert_details(&server.signed("PUT", &signed, b""), "Missing copy source");
    absent(&server, "goog4");
}

#[test]
fn amz_headers_without_a_recorded_answer_stay_rejected_on_other_routes() {
    let (_dir, server, _key) = fixture();
    let with_acl = format!("{ADMIN}x-amz-acl: private\r\n");
    server
        .request("PUT", "/test-bucket/source", ADMIN, b"payload")
        .expect(200);
    error(
        &server.request("GET", "/test-bucket/source", &with_acl, b""),
        400,
        "InvalidArgument",
    );
    error(
        &server.request("DELETE", "/test-bucket/source", &with_acl, b""),
        400,
        "InvalidArgument",
    );
    error(
        &server.request(
            "PUT",
            "/new-bucket",
            &format!("{with_acl}x-goog-project-id: local\r\n"),
            b"",
        ),
        400,
        "InvalidArgument",
    );
    server
        .request("HEAD", "/new-bucket", ADMIN, b"")
        .expect(404);
}

#[test]
fn aws4_ignores_amz_headers_without_an_equivalent() {
    let (_dir, server, key) = fixture();
    for (index, extras) in [
        [("x-amz-server-side-encryption", "AES256")].as_slice(),
        [("x-amz-no-such-header", "1")].as_slice(),
        [("x-amz-tagging", "a=b")].as_slice(),
        [("x-amz-metadata-directive", "REPLACE")].as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("ignored-{index}");
        aws4_put(&server, &key, &name, extras).expect(200);
        assert!(object(&server, &name).get("metadata").is_none(), "{name}");
    }
}

#[test]
fn aws4_reads_answer_with_amz_custom_metadata() {
    let (_dir, server, key) = fixture();
    server
        .request(
            "PUT",
            "/test-bucket/spelled",
            &format!("{ADMIN}x-goog-meta-origin: goog\r\n"),
            b"payload",
        )
        .expect(200);
    for method in ["GET", "HEAD"] {
        let signed = sign(
            &key,
            method,
            "/test-bucket/spelled",
            true,
            true,
            &[],
            SystemTime::now(),
        );
        let reply = server.signed(method, &signed, b"");
        reply.expect(200);
        assert_eq!(reply.headers["x-amz-meta-origin"], "goog", "{method}");
        assert!(
            !reply.headers.contains_key("x-goog-meta-origin"),
            "{method}"
        );
    }
    let oauth = server.request("HEAD", "/test-bucket/spelled", ADMIN, b"");
    oauth.expect(200);
    assert_eq!(oauth.headers["x-goog-meta-origin"], "goog");
    assert!(!oauth.headers.contains_key("x-amz-meta-origin"));
}

#[test]
fn aws4_refuses_conflicting_and_grant_headers_without_publishing() {
    let (_dir, server, key) = fixture();
    for (index, extras) in [
        [("x-amz-meta-k", "a"), ("x-goog-meta-k", "b")].as_slice(),
        [("x-amz-acl", "private"), ("x-goog-acl", "private")].as_slice(),
        [
            ("x-amz-storage-class", "STANDARD"),
            ("x-goog-storage-class", "STANDARD"),
        ]
        .as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("both-{index}");
        error(
            &aws4_put(&server, &key, &name, extras),
            400,
            "ExcessHeaderValues",
        );
        absent(&server, &name);
    }
    for (index, extras) in [
        [("x-amz-grant-read", "id=x")].as_slice(),
        [("x-amz-server-side-encryption-customer-key", "a")].as_slice(),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("refused-{index}");
        error(
            &aws4_put(&server, &key, &name, extras),
            400,
            "InvalidArgument",
        );
        absent(&server, &name);
    }
    assert_details(
        &aws4_put(
            &server,
            &key,
            "s3-canned",
            &[("x-amz-acl", "aws-exec-read")],
        ),
        "Invalid canned acl: aws-exec-read",
    );
    absent(&server, "s3-canned");
    error(
        &server.request(
            "PUT",
            "/test-bucket/oauth-unmapped",
            &format!("{ADMIN}x-amz-tagging: a=b\r\n"),
            b"payload",
        ),
        400,
        "InvalidArgument",
    );
    absent(&server, "oauth-unmapped");
}

#[test]
fn aws4_tampered_or_unsigned_amz_headers_fail_verification() {
    let (_dir, server, key) = fixture();
    let signed = sign(
        &key,
        "PUT",
        "/test-bucket/tampered",
        true,
        true,
        &[("x-amz-meta-k", "a")],
        SystemTime::now(),
    );
    let tampered = Signed {
        path: signed.path.clone(),
        headers: signed.headers.replace("x-amz-meta-k: a", "x-amz-meta-k: b"),
    };
    error(
        &server.signed("PUT", &tampered, b"payload"),
        403,
        "SignatureDoesNotMatch",
    );
    absent(&server, "tampered");

    let unsigned = Signed {
        path: signed.path.clone(),
        headers: format!("x-amz-acl: public-read\r\n{}", signed.headers),
    };
    error(
        &server.signed("PUT", &unsigned, b"payload"),
        400,
        "InvalidAuthentication",
    );
    absent(&server, "tampered");
}

#[test]
fn aws_v2_signature_matches_aws4_for_amz_metadata_class_acl_and_copy() {
    let (_dir, server, key) = fixture();
    let extras = [
        ("x-amz-acl", "bucket-owner-read"),
        ("x-amz-meta-origin", "s3"),
        ("x-amz-storage-class", "NEARLINE"),
    ];
    aws4_put(&server, &key, "aws4", &extras).expect(200);
    let path = "/test-bucket/aws-v2";
    let v2 = xml_v2_hmac::header(&key, "AWS", "PUT", path, path, &extras);
    xml_v2_hmac::send(&server, "PUT", &v2, b"", "localhost").expect(200);
    let (aws4, aws_v2) = (object(&server, "aws4"), object(&server, "aws-v2"));
    assert_eq!(aws_v2["metadata"], aws4["metadata"]);
    assert_eq!(aws_v2["storageClass"], aws4["storageClass"]);
    assert_eq!(acl(&aws_v2), acl(&aws4));

    let path = "/test-bucket/aws-v2-copy";
    let copy = xml_v2_hmac::header(
        &key,
        "AWS",
        "PUT",
        path,
        path,
        &[
            ("x-amz-copy-source", "/test-bucket/aws4"),
            ("x-amz-metadata-directive", "REPLACE"),
            ("x-amz-meta-copied", "yes"),
        ],
    );
    xml_v2_hmac::send(&server, "PUT", &copy, b"", "localhost").expect(200);
    assert_eq!(
        object(&server, "aws-v2-copy")["metadata"],
        json!({"copied":"yes"})
    );
}

#[test]
fn permissive_server_reads_amz_headers_by_signature_form() {
    let dir = TestDir::new();
    let server = Server::launch(
        http_command(&dir.0.join("data")).args(["--api-base-url", "http://127.0.0.1:4443"]),
    );
    server.bucket("test-bucket");
    // Signatures are ignored here, so a made-up key still selects the AWS meaning.
    let made_up = Key {
        id: "made-up".into(),
        secret: "made-up".into(),
    };
    let signed = sign(
        &made_up,
        "PUT",
        "/test-bucket/signed",
        true,
        true,
        &[("x-amz-meta-origin", "s3")],
        SystemTime::now(),
    );
    server.signed("PUT", &signed, b"payload").expect(200);
    assert_eq!(
        object(&server, "signed")["metadata"],
        json!({"origin":"s3"})
    );

    server
        .request(
            "PUT",
            "/test-bucket/anonymous",
            "x-amz-meta-origin: s3\r\n",
            b"payload",
        )
        .expect(200);
    assert_eq!(object(&server, "anonymous").get("metadata"), None);
    assert_details(
        &server.request(
            "PUT",
            "/test-bucket/anonymous-copy",
            "x-amz-copy-source: /test-bucket/signed\r\n",
            b"",
        ),
        "Missing copy source",
    );
    absent(&server, "anonymous-copy");
}
