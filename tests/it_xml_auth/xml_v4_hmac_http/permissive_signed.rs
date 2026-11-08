//! Permissive mode with `signed_urls=verify` verifies signed XML requests exactly as strict mode
//! does, then grants permissive access: no signer permission is ever checked.

use super::*;
use gcs_mimic::auth::{
    signature::{SignUrlRequest, sign_policy, sign_url},
    test_signer,
};
use hyper::HeaderMap;

fn permissive(dir: &TestDir, options: &[&str]) -> Server {
    Server::launch(
        http_command(&dir.0.join("data"))
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .args(options),
    )
}

/// A V4 RSA query signature made with the published test key.
fn rsa_v4(method: &str, path: &str, signer: &str, now: SystemTime) -> Signed {
    let url = sign_url(SignUrlRequest {
        url: &format!("http://localhost{path}"),
        method: &method.parse().unwrap(),
        signer,
        private_key: PRIVATE,
        expires: 60,
        now,
        headers: HeaderMap::new(),
    })
    .unwrap();
    Signed {
        path: url.strip_prefix("http://localhost").unwrap().into(),
        headers: String::new(),
    }
}

/// A V2 RSA query signature made with the published test key, valid until `expires`.
fn rsa_v2(method: &str, path: &str, signer: &str, expires: SystemTime) -> Signed {
    let expires = expires
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let hex = sign_policy(&format!("{method}\n\n\n{expires}\n{path}"), PRIVATE).unwrap();
    let bytes: Vec<_> = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
        .collect();
    Signed {
        path: format!(
            "{path}?GoogleAccessId={}&Expires={expires}&Signature={}",
            encode_query_component(signer),
            encode_query_component(&xml_v2_hmac::base64(&bytes))
        ),
        headers: String::new(),
    }
}

/// XML requests without the JSON Content-Type the default client adds, which V2 would sign.
fn bare(server: &Server, method: &str, signed: &Signed, body: &[u8]) -> Reply {
    xml_v2_hmac::send(server, method, signed, body, "localhost")
}

fn body_contains(reply: &Reply, text: &str) -> bool {
    String::from_utf8_lossy(&reply.body).contains(text)
}

fn unsigned(path: &str) -> Signed {
    Signed {
        path: path.into(),
        headers: String::new(),
    }
}

#[test]
fn verified_signatures_get_permissive_access_without_any_grant() {
    let dir = TestDir::new();
    let server = permissive(&dir, &["--signed-urls", "verify"]);
    assert_eq!(server.ready["auth_mode"], "permissive");
    assert_eq!(server.ready["signed_urls"], "verify");
    assert_eq!(server.ready["test_signer"], "enabled");
    assert!(server.ready.get("identities").is_none());
    server.bucket("test-bucket");
    // No identity names this service account; an ACTIVE key is enough when nothing is
    // authorized by identity.
    let key = server.key("unregistered@example.invalid");
    let now = SystemTime::now();
    for aws in [false, true] {
        for header in [false, true] {
            let path = format!("/test-bucket/hmac-{aws}-{header}");
            let put = sign(&key, "PUT", &path, aws, header, &[], now);
            server.signed("PUT", &put, b"hmac bytes").expect(200);
            let get = sign(&key, "GET", &path, aws, header, &[], now);
            let reply = server.signed("GET", &get, b"");
            reply.expect(200);
            assert_eq!(reply.body, b"hmac bytes");
        }
    }
    let v2_hmac = xml_v2_hmac::query(&key, "GET", "/test-bucket/hmac-false-false", &[]);
    assert_eq!(bare(&server, "GET", &v2_hmac, b"").body, b"hmac bytes");
    let put = rsa_v4("PUT", "/test-bucket/rsa", test_signer::ID, now);
    bare(&server, "PUT", &put, b"rsa bytes").expect(200);
    let get = rsa_v4("GET", "/test-bucket/rsa", test_signer::ID, now);
    assert_eq!(bare(&server, "GET", &get, b"").body, b"rsa bytes");
    let v2 = rsa_v2(
        "GET",
        "/test-bucket/rsa",
        test_signer::ID,
        now + Duration::from_secs(60),
    );
    let reply = bare(&server, "GET", &v2, b"");
    reply.expect(200);
    assert_eq!(reply.body, b"rsa bytes");
    let signed = xml_v4_streaming::headers(&key, "/test-bucket/chunked", 3, Some("aws-chunked"));
    let body = xml_v4_streaming::chain(&key, &signed, &[b"abc"]);
    xml_v4_streaming::send(&server, &signed, &body, None, false).expect(200);
    // Unsigned XML and JSON requests keep ignoring credentials.
    assert_eq!(
        bare(&server, "GET", &unsigned("/test-bucket/chunked"), b"").body,
        b"abc"
    );
    let reply = server.request(
        "GET",
        "/storage/v1/b/test-bucket/o/rsa?alt=media",
        "Authorization: Bearer not-a-registered-token\r\n",
        b"",
    );
    reply.expect(200);
    assert_eq!(reply.body, b"rsa bytes");
}

#[test]
fn rejected_signatures_get_the_strict_errors_and_change_nothing() {
    let dir = TestDir::new();
    let server = permissive(&dir, &["--signed-urls", "verify"]);
    server.bucket("test-bucket");
    let key = server.key("unregistered@example.invalid");
    let now = SystemTime::now();
    let mut tampered = sign(&key, "PUT", "/test-bucket/signed", false, false, &[], now);
    tampered.path = tampered.path.replace("/signed?", "/other?");
    error(
        &server.signed("PUT", &tampered, b"x"),
        403,
        "SignatureDoesNotMatch",
    );
    let expired = sign(
        &key,
        "PUT",
        "/test-bucket/expired",
        false,
        false,
        &[],
        now - Duration::from_secs(120),
    );
    error(&server.signed("PUT", &expired, b"x"), 400, "ExpiredToken");
    let mut changed = sign(
        &key,
        "PUT",
        "/test-bucket/changed",
        false,
        true,
        &[("x-goog-meta-version", "one")],
        now,
    );
    changed.headers = changed.headers.replace("version: one", "version: two");
    error(
        &server.signed("PUT", &changed, b"x"),
        403,
        "SignatureDoesNotMatch",
    );
    let unknown = Key {
        id: "GOOG1UNKNOWNACCESSKEY".into(),
        secret: key.secret.clone(),
    };
    let unknown_rsa = rsa_v4(
        "PUT",
        "/test-bucket/unknown",
        "unknown@example.invalid",
        now,
    );
    // Nothing is registered in permissive mode, so the answer names where keys come from.
    for reply in [false, true]
        .map(|aws| {
            let signed = sign(&unknown, "PUT", "/test-bucket/unknown", aws, true, &[], now);
            server.signed("PUT", &signed, b"x")
        })
        .into_iter()
        .chain([bare(&server, "PUT", &unknown_rsa, b"x")])
    {
        error(&reply, 403, "InvalidAccessKeyId");
        assert!(
            body_contains(
                &reply,
                "keys come from the public_keys of the identity document (identity_file or identities), HMAC keys created through the JSON API and the built-in test signer"
            ),
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
    }
    let expired_v2 = rsa_v2(
        "PUT",
        "/test-bucket/expired",
        test_signer::ID,
        now - Duration::from_secs(1),
    );
    error(
        &bare(&server, "PUT", &expired_v2, b"x"),
        400,
        "ExpiredToken",
    );
    let mut tampered_v2 = rsa_v2(
        "PUT",
        "/test-bucket/signed",
        test_signer::ID,
        now + Duration::from_secs(60),
    );
    tampered_v2.path = tampered_v2.path.replace("/signed?", "/other?");
    error(
        &bare(&server, "PUT", &tampered_v2, b"x"),
        403,
        "SignatureDoesNotMatch",
    );
    let signed = xml_v4_streaming::headers(&key, "/test-bucket/chunked", 3, Some("aws-chunked"));
    let wrong = Key {
        id: key.id,
        secret: "a different secret".into(),
    };
    let body = xml_v4_streaming::chain(&wrong, &signed, &[b"abc"]);
    let reply = xml_v4_streaming::send(&server, &signed, &body, None, false);
    error(&reply, 403, "SignatureDoesNotMatch");
    for name in [
        "signed", "other", "expired", "changed", "unknown", "chunked",
    ] {
        bare(
            &server,
            "GET",
            &unsigned(&format!("/test-bucket/{name}")),
            b"",
        )
        .expect(404);
    }
}

#[test]
fn identity_file_keys_verify_and_a_disabled_test_signer_is_unknown() {
    let dir = TestDir::new();
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[{"id":"file-signer@example.invalid","tokens":[],"public_keys":[PUBLIC]}]}),
    );
    let server = permissive(
        &dir,
        &[
            "--signed-urls",
            "verify",
            "--test-signer",
            "disabled",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    assert_eq!(server.ready["auth_mode"], "permissive");
    assert_eq!(server.ready["test_signer"], "disabled");
    assert!(server.ready.get("identities").is_none());
    server.bucket("test-bucket");
    let now = SystemTime::now();
    let put = rsa_v4(
        "PUT",
        "/test-bucket/file",
        "file-signer@example.invalid",
        now,
    );
    bare(&server, "PUT", &put, b"file bytes").expect(200);
    let get = rsa_v4(
        "GET",
        "/test-bucket/file",
        "file-signer@example.invalid",
        now,
    );
    assert_eq!(bare(&server, "GET", &get, b"").body, b"file bytes");
    let built_in = rsa_v4("GET", "/test-bucket/file", test_signer::ID, now);
    let reply = bare(&server, "GET", &built_in, b"");
    error(&reply, 403, "InvalidAccessKeyId");
    assert!(body_contains(
        &reply,
        "test-signer@gcs-mimic.invalid is the built-in test signer, which is disabled; set test_signer=enabled."
    ));
}

#[test]
fn the_default_ignore_profile_still_accepts_unverifiable_signatures() {
    let dir = TestDir::new();
    let server = permissive(&dir, &[]);
    assert_eq!(server.ready["signed_urls"], "ignore");
    assert_eq!(server.ready["test_signer"], "disabled");
    server.bucket("test-bucket");
    let now = SystemTime::now();
    bare(&server, "PUT", &unsigned("/test-bucket/object"), b"bytes").expect(200);
    let unknown = Key {
        id: "GOOG1UNKNOWNACCESSKEY".into(),
        secret: "secret".into(),
    };
    for signed in [
        sign(
            &unknown,
            "GET",
            "/test-bucket/object",
            false,
            false,
            &[],
            now - Duration::from_secs(120),
        ),
        rsa_v4("GET", "/test-bucket/object", "unknown@example.invalid", now),
        rsa_v2(
            "GET",
            "/test-bucket/object",
            "unknown@example.invalid",
            now - Duration::from_secs(1),
        ),
    ] {
        assert_eq!(bare(&server, "GET", &signed, b"").body, b"bytes");
    }
    // A signed aws-chunked upload carries chunk signatures that only a verified key can check.
    let key = server.key("unregistered@example.invalid");
    let signed = xml_v4_streaming::headers(&key, "/test-bucket/chunked", 3, Some("aws-chunked"));
    let body = xml_v4_streaming::chain(&key, &signed, &[b"abc"]);
    let reply = xml_v4_streaming::send(&server, &signed, &body, None, false);
    error(&reply, 403, "InvalidAccessKeyId");
    assert!(body_contains(
        &reply,
        "because signatures are ignored; set signed_urls=verify"
    ));
}

#[test]
fn startup_pairs_identity_files_and_signed_urls_with_the_profile() {
    let dir = TestDir::new();
    let file = |name: &str, document: serde_json::Value| {
        let parent = dir.0.join(name);
        std::fs::create_dir(&parent).unwrap();
        common::process::write_identities(&parent, &document)
            .to_str()
            .unwrap()
            .to_owned()
    };
    let keys_only = file(
        "keys",
        json!({"identities":[{"id":"signer@example.invalid","public_keys":[PUBLIC]}]}),
    );
    let principals = file(
        "principals",
        json!({"identities":[{"id":"admin@example.invalid","tokens":["admin"]}]}),
    );
    let rejected = |field: &str, entry: serde_json::Value| {
        let mut entry = entry;
        entry["id"] = json!("signer@example.invalid");
        entry["public_keys"] = json!([PUBLIC]);
        (file(field, json!({"identities":[entry]})), field.to_owned())
    };
    let roles = rejected(
        "project_roles",
        json!({"project_roles":{"local":["roles/storage.admin"]}}),
    );
    let alias = rejected(
        "acl_entity",
        json!({"acl_entity":"user-alias@example.invalid"}),
    );
    let tokens = rejected("tokens", json!({"tokens":["token"]}));
    let pap = (
        file(
            "pap",
            json!({"identities":[{"id":"signer@example.invalid","public_keys":[PUBLIC]}],
                "public_access_prevention_projects":["local"]}),
        ),
        "public_access_prevention_projects".to_owned(),
    );
    let missing_keys = file(
        "missing",
        json!({"identities":[{"id":"signer@example.invalid","tokens":[]}]}),
    );
    let mut cases: Vec<(Vec<&str>, String)> = vec![
        (
            vec!["--identity-file", &keys_only],
            "identity_file requires auth_mode=strict, or signed_urls=verify".into(),
        ),
        (
            vec!["--identity-file", &keys_only, "--signed-urls", "ignore"],
            "identity_file requires auth_mode=strict, or signed_urls=verify".into(),
        ),
        (
            vec![
                "--auth-mode",
                "strict",
                "--identity-file",
                &principals,
                "--signed-urls",
                "ignore",
            ],
            "signed_urls=ignore requires auth_mode=permissive".into(),
        ),
        (
            vec!["--signed-urls", "sometimes"],
            "signed_urls must be ignore or verify".into(),
        ),
        (
            vec!["--signed-urls", "verify", "--identity-file", &missing_keys],
            "missing required field: public_keys".into(),
        ),
        (
            vec!["--test-signer", "enabled"],
            "test_signer=enabled has no effect while signatures are ignored".into(),
        ),
    ];
    for (path, field) in [&roles, &alias, &tokens, &pap] {
        cases.push((
            vec!["--signed-urls", "verify", "--identity-file", path],
            format!("{field} requires auth_mode=strict"),
        ));
    }
    for (args, message) in &cases {
        let output = common::server_command()
            .args(["serve", "--scheme", "http", "--listen", "127.0.0.1:0"])
            .args(["--data-dir", dir.0.join("data").to_str().unwrap()])
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}: no readiness");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message.as_str()), "{args:?}: {stderr}");
    }
    for args in [
        &["--signed-urls", "verify", "--identity-file", &keys_only][..],
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            &principals,
            "--signed-urls",
            "verify",
        ],
        &[
            "--signed-urls",
            "verify",
            "--identity-file",
            &keys_only,
            "--local-creator-email",
            "writer@example.invalid",
        ],
    ] {
        let data = TestDir::new();
        let server = permissive(&data, args);
        assert_eq!(server.ready["signed_urls"], "verify", "{args:?}");
    }
    // An explicit disabled is already true where nothing verifies, so it stays accepted.
    let data = TestDir::new();
    let server = permissive(&data, &["--test-signer", "disabled"]);
    assert_eq!(server.ready["signed_urls"], "ignore");
    assert_eq!(server.ready["test_signer"], "disabled");
}
