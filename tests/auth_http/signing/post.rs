use super::*;
use gcs_mimic::auth::signature::{sign_policy, timestamp};
use std::{
    sync::LazyLock,
    time::{Duration, SystemTime},
};

// Strict mode bounds a verified policy by x-goog-date, like a signed URL, so the forms are
// signed at the real current time. One value per run keeps the credential scope consistent.
static DATE: LazyLock<String> = LazyLock::new(|| timestamp(SystemTime::now()).unwrap());
static FUTURE: LazyLock<String> =
    LazyLock::new(|| timestamp(SystemTime::now() + Duration::from_secs(3600)).unwrap());
const BOUNDARY: &str = "form-boundary";

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            result.push(if index <= chunk.len() {
                ALPHABET[(value >> (18 - 6 * index) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    result
}

fn credential(signer: &str) -> String {
    format!("{signer}/{}/auto/storage/goog4_request", &DATE[..8])
}

/// Build the fields the official SDKs send: the policy covers bucket, key and the
/// x-goog-* credential fields, plus any extra fields given here.
fn policy_form(
    signer: &str,
    key: &str,
    expiration: &str,
    extra: &[(&str, &str)],
    conditions: &[Value],
) -> Vec<(String, String)> {
    let mut all = vec![
        json!({"bucket": "iam-bucket"}),
        json!({"key": key}),
        json!({"x-goog-date": *DATE}),
        json!({"x-goog-credential": credential(signer)}),
        json!({"x-goog-algorithm": "GOOG4-RSA-SHA256"}),
    ];
    all.extend(extra.iter().map(|(name, value)| json!({ *name: value })));
    all.extend_from_slice(conditions);
    let policy = base64(
        json!({"conditions": all, "expiration": expiration})
            .to_string()
            .as_bytes(),
    );
    let signature = sign_policy(&policy, PRIVATE).unwrap();
    let mut fields: Vec<(String, String)> = vec![
        ("key".into(), key.into()),
        ("x-goog-date".into(), DATE.clone()),
        ("x-goog-credential".into(), credential(signer)),
        ("x-goog-algorithm".into(), "GOOG4-RSA-SHA256".into()),
    ];
    fields.extend(
        extra
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into())),
    );
    fields.push(("policy".into(), policy));
    fields.push(("x-goog-signature".into(), signature));
    fields
}

fn body(fields: &[(String, String)], filename: &str, file: &[u8], after: &str) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n{after}--{BOUNDARY}--\r\n").as_bytes());
    body
}

fn post(server: &Server, path: &str, fields: &[(String, String)], file: &[u8]) -> Reply {
    server.request(
        "POST",
        path,
        &format!("Content-Type: multipart/form-data; boundary={BOUNDARY}\r\n"),
        &body(fields, "post.txt", file, ""),
    )
}

fn missing(server: &Server, name: &str) {
    error(
        &server.get(ADMIN, &format!("/iam-bucket/{name}")),
        404,
        "NoSuchKey",
    );
}

#[test]
fn v4_policy_upload_streams_through_the_shared_write_path_with_signer_permissions() {
    let (_config, _data, server) = fixture();
    let fields = policy_form(
        "creator@example.invalid",
        "uploads/${filename}",
        &FUTURE,
        &[
            ("Content-Type", "text/plain"),
            ("x-goog-meta-owner", "alice"),
            ("Cache-Control", "no-cache"),
        ],
        &[],
    );
    let reply = post(&server, "/iam-bucket", &fields, b"posted bytes");
    assert_eq!(
        reply.status,
        204,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert!(reply.headers["x-goog-generation"].parse::<i64>().unwrap() > 0);
    let stored = server.get(ADMIN, "/iam-bucket/uploads/post.txt");
    assert_eq!(stored.ok().body, b"posted bytes");
    assert_eq!(stored.headers["etag"], reply.headers["etag"]);
    let metadata = server
        .get(ADMIN, "/storage/v1/b/iam-bucket/o/uploads%2Fpost.txt")
        .ok()
        .json();
    assert_eq!(metadata["contentType"], "text/plain");
    assert_eq!(metadata["cacheControl"], "no-cache");
    assert_eq!(metadata["metadata"], json!({"owner":"alice"}));
    assert_eq!(metadata["generation"], reply.headers["x-goog-generation"]);

    // objectCreator has no delete permission, so the same signer cannot overwrite.
    error(
        &post(&server, "/iam-bucket", &fields, b"replaced"),
        403,
        "AccessDenied",
    );
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/uploads/post.txt").ok().body,
        b"posted bytes"
    );
    // A signer without create permission is denied before any object is written.
    let viewer = policy_form("viewer@example.invalid", "viewer.txt", &FUTURE, &[], &[]);
    error(
        &post(&server, "/iam-bucket", &viewer, b"x"),
        403,
        "AccessDenied",
    );
    missing(&server, "viewer.txt");
    // Without a policy the form is anonymous and strict mode requires a public grant.
    let anonymous = vec![("key".to_owned(), "anonymous.txt".to_owned())];
    error(
        &post(&server, "/iam-bucket", &anonymous, b"x"),
        401,
        "AuthenticationRequired",
    );
    missing(&server, "anonymous.txt");
}

#[test]
fn v4_policy_success_status_201_xml_200_and_redirect() {
    let (_config, _data, server) = fixture();
    let created = post(
        &server,
        "/iam-bucket",
        &policy_form(
            "admin@example.invalid",
            "created & <named>.txt",
            &FUTURE,
            &[("success_action_status", "201")],
            &[],
        ),
        b"one",
    );
    assert_eq!(created.status, 201);
    assert_eq!(
        created.headers["content-type"],
        "application/xml; charset=UTF-8"
    );
    let text = String::from_utf8(created.body.clone()).unwrap();
    assert!(text.contains("<Bucket>iam-bucket</Bucket>"), "{text}");
    assert!(
        text.contains("<Key>created &amp; &lt;named&gt;.txt</Key>"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "<ETag>{}</ETag>",
            created.headers["etag"].replace('"', "&quot;")
        )),
        "{text}"
    );
    assert!(
        text.contains(
            "<Location>http://127.0.0.1:4443/iam-bucket/created%20%26%20%3Cnamed%3E.txt</Location>"
        ),
        "{text}"
    );
    let ok = post(
        &server,
        "/iam-bucket",
        &policy_form(
            "admin@example.invalid",
            "ok.txt",
            &FUTURE,
            &[("success_action_status", "200")],
            &[],
        ),
        b"two",
    );
    assert_eq!((ok.status, ok.body.len()), (200, 0));
    let redirect = post(
        &server,
        "/iam-bucket",
        &policy_form(
            "admin@example.invalid",
            "redirect.txt",
            &FUTURE,
            &[(
                "success_action_redirect",
                "https://app.example.invalid/done?x=1",
            )],
            &[],
        ),
        b"three",
    );
    assert_eq!(redirect.status, 303);
    assert_eq!(
        redirect.headers["location"],
        "https://app.example.invalid/done?x=1"
    );
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/redirect.txt").ok().body,
        b"three"
    );
}

#[test]
fn v4_policy_rejects_tampering_unknown_signers_expiry_and_condition_violations() {
    let (_config, _data, server) = fixture();
    let valid = policy_form("admin@example.invalid", "rejected.txt", &FUTURE, &[], &[]);
    let replace = |name: &str, value: &str| -> Vec<(String, String)> {
        valid
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    if k == name {
                        value.to_owned()
                    } else {
                        v.clone()
                    },
                )
            })
            .collect()
    };
    // A policy signed for another key cannot be reused: its signature no longer matches.
    let other = policy_form("admin@example.invalid", "other.txt", &FUTURE, &[], &[]);
    let other_policy = other.iter().find(|(k, _)| k == "policy").unwrap().1.clone();
    for (fields, status, code) in [
        (
            replace("policy", &other_policy),
            403,
            "SignatureDoesNotMatch",
        ),
        (
            replace("x-goog-signature", "00ff"),
            403,
            "SignatureDoesNotMatch",
        ),
        (
            replace("x-goog-credential", &credential("nobody@example.invalid")),
            403,
            "InvalidAccessKeyId",
        ),
        (
            replace("x-goog-algorithm", "GOOG4-HMAC-SHA256"),
            400,
            "InvalidArgument",
        ),
        (
            replace("x-goog-date", "20200101T000000Z"),
            400,
            "InvalidArgument",
        ),
        (replace("key", "changed.txt"), 400, "InvalidPolicyDocument"),
        (
            policy_form(
                "admin@example.invalid",
                "rejected.txt",
                "2020-01-01T00:00:00Z",
                &[],
                &[],
            ),
            400,
            "InvalidPolicyDocument",
        ),
        (
            [
                valid.clone(),
                vec![("x-goog-meta-extra".into(), "x".into())],
            ]
            .concat(),
            400,
            "InvalidPolicyDocument",
        ),
        (
            policy_form(
                "admin@example.invalid",
                "rejected.txt",
                &FUTURE,
                &[],
                &[json!(["starts-with", "$Content-Type", "image/"])],
            ),
            400,
            "InvalidPolicyDocument",
        ),
    ] {
        error(&post(&server, "/iam-bucket", &fields, b"x"), status, code);
    }
    missing(&server, "rejected.txt");
    missing(&server, "changed.txt");
    // The bucket condition binds the request target.
    server
        .create_bucket(ADMIN, "project-a", "second-bucket")
        .ok();
    error(
        &post(&server, "/second-bucket", &valid, b"x"),
        400,
        "InvalidPolicyDocument",
    );
    error(
        &server.get(ADMIN, "/second-bucket/rejected.txt"),
        404,
        "NoSuchKey",
    );
}

#[test]
fn v4_policy_content_length_range_bounds_are_inclusive_and_leave_no_object() {
    let (_config, _data, server) = fixture();
    for (key, file, status, code) in [
        ("small.txt", b"ab".as_slice(), 400, Some("EntityTooSmall")),
        ("min.txt", b"abc", 204, None),
        ("max.txt", b"abcde", 204, None),
        ("large.txt", b"abcdef", 400, Some("EntityTooLarge")),
    ] {
        let fields = policy_form(
            "admin@example.invalid",
            key,
            &FUTURE,
            &[],
            &[json!(["content-length-range", 3, 5])],
        );
        let reply = post(&server, "/iam-bucket", &fields, file);
        match code {
            Some(code) => {
                error(&reply, status, code);
                missing(&server, key);
            }
            None => {
                assert_eq!(reply.status, status);
                assert_eq!(
                    server.get(ADMIN, &format!("/iam-bucket/{key}")).ok().body,
                    file
                );
            }
        }
    }
}

#[test]
fn post_object_framing_errors_and_other_routes_stay_rejected() {
    let (_config, _data, server) = fixture();
    let fields = policy_form("admin@example.invalid", "framed.txt", &FUTURE, &[], &[]);
    // The file must be the last field; a trailing field aborts the upload.
    let trailing = body(
        &fields,
        "post.txt",
        b"x",
        &format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"late\"\r\n\r\nv\r\n"),
    );
    let reply = server.request(
        "POST",
        "/iam-bucket",
        &format!("Content-Type: multipart/form-data; boundary={BOUNDARY}\r\n"),
        &trailing,
    );
    error(&reply, 400, "InvalidArgument");
    missing(&server, "framed.txt");
    for (path, content_type) in [
        (
            "/iam-bucket?uploads",
            "multipart/form-data; boundary=form-boundary",
        ),
        ("/iam-bucket", "multipart/related; boundary=form-boundary"),
        ("/iam-bucket", "application/x-www-form-urlencoded"),
    ] {
        let reply = server.request(
            "POST",
            path,
            &format!("Content-Type: {content_type}\r\n"),
            &body(&fields, "post.txt", b"x", ""),
        );
        error(&reply, 400, "InvalidArgument");
    }
    missing(&server, "framed.txt");
    // POST on an object path is still not an XML operation.
    error(
        &post(&server, "/iam-bucket/object", &fields, b"x"),
        400,
        "InvalidArgument",
    );
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/object").ok().body,
        b"hello world"
    );
}

#[test]
fn v4_policy_signer_uploads_under_public_access_prevention() {
    let (_config, _data, server) = fixture();
    server
        .request(
            "PATCH",
            "/storage/v1/b/iam-bucket",
            ADMIN,
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .ok();
    let fields = policy_form("creator@example.invalid", "pap.txt", &FUTURE, &[], &[]);
    assert_eq!(post(&server, "/iam-bucket", &fields, b"pap").status, 204);
    assert_eq!(server.get(ADMIN, "/iam-bucket/pap.txt").ok().body, b"pap");
}

#[test]
fn v4_policy_upload_targets_a_virtual_host_bucket_root() {
    let config = TestDir::new();
    let data = TestDir::new();
    let mut identities = registry();
    for identity in identities["identities"].as_array_mut().unwrap() {
        identity["public_keys"] = json!([PUBLIC]);
    }
    let file = config.0.join("identities.json");
    std::fs::write(&file, identities.to_string()).unwrap();
    let (server, _) = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            file.to_str().unwrap(),
            "--xml-virtual-host-domains",
            "localhost",
        ],
    );
    server.create_bucket(ADMIN, "project-a", "iam-bucket").ok();
    let fields = policy_form("admin@example.invalid", "virtual.txt", &FUTURE, &[], &[]);
    let form = body(&fields, "post.txt", b"virtual", "");
    let mut request = format!(
        "POST / HTTP/1.1\r\nHost: iam-bucket.localhost\r\nContent-Length: {}\r\nContent-Type: multipart/form-data; boundary={BOUNDARY}\r\n\r\n",
        form.len()
    )
    .into_bytes();
    request.extend_from_slice(&form);
    assert_eq!(server.raw(&request).status, 204);
    assert_eq!(
        server.get(ADMIN, "/iam-bucket/virtual.txt").ok().body,
        b"virtual"
    );
}

#[test]
fn permissive_profile_ignores_policy_credentials_but_enforces_conditions() {
    let data = TestDir::new();
    let (server, _) = Server::start(&data, &[]);
    server.create_bucket("", "project-a", "iam-bucket").ok();
    let mut fields = policy_form("anyone@example.invalid", "open.txt", &FUTURE, &[], &[]);
    fields.last_mut().unwrap().1 = "00".into();
    assert_eq!(post(&server, "/iam-bucket", &fields, b"open").status, 204);
    assert_eq!(server.get("", "/iam-bucket/open.txt").ok().body, b"open");
    let expired = policy_form(
        "anyone@example.invalid",
        "expired.txt",
        "2020-01-01T00:00:00Z",
        &[],
        &[],
    );
    error(
        &post(&server, "/iam-bucket", &expired, b"x"),
        400,
        "InvalidPolicyDocument",
    );
    let mut changed = policy_form("anyone@example.invalid", "a.txt", &FUTURE, &[], &[]);
    changed[0].1 = "b.txt".into();
    error(
        &post(&server, "/iam-bucket", &changed, b"x"),
        400,
        "InvalidPolicyDocument",
    );
    error(&server.get("", "/iam-bucket/b.txt"), 404, "NoSuchKey");
}
