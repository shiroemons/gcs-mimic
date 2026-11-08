use super::*;
use gcs_mimic::auth::{
    signature::{encode_query_component, sign_policy},
    test_signer,
};
use std::time::UNIX_EPOCH;

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .chain([0, 0].iter())
            .take(3)
            .fold(0u32, |value, byte| value << 8 | u32::from(*byte));
        for index in 0..4 {
            encoded.push(if index > chunk.len() {
                '='
            } else {
                ALPHABET[(value >> (18 - 6 * index) & 63) as usize] as char
            });
        }
    }
    encoded
}

/// A V2 RSA query signature from the built-in signer's published key.
fn v2(server: &Server, path: &str) -> String {
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 60;
    let hex = sign_policy(
        &format!("GET\n\n\n{expires}\n{path}"),
        test_signer::PRIVATE_KEY_PEM,
    )
    .unwrap();
    let bytes: Vec<_> = hex
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    format!(
        "http://{}{path}?GoogleAccessId={}&Expires={expires}&Signature={}",
        server.address,
        encode_query_component(test_signer::ID),
        encode_query_component(&base64(&bytes))
    )
}

fn v4(server: &Server, path: &str) -> String {
    sign(
        server,
        &Method::GET,
        path,
        test_signer::ID,
        SystemTime::now(),
        HeaderMap::new(),
    )
}

/// `gcs-mimic url sign` without `--signer` and `--private-key`.
fn cli_signed(server: &Server, object: &str) -> String {
    let output = common::server_command()
        .args([
            "url",
            "sign",
            "--base-url",
            &format!("http://{}", server.address),
            "--bucket",
            "iam-bucket",
            "--object",
            object,
            "--method",
            "GET",
            "--expires",
            "60",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn an_undeclared_built_in_signer_verifies_v4_and_v2_but_needs_a_bucket_grant() {
    let (_config, _data, server) = fixture();
    let urls = [
        v4(&server, "/iam-bucket/object"),
        v2(&server, "/iam-bucket/object"),
        cli_signed(&server, "object"),
    ];
    for url in &urls {
        error(&send(&server, "GET", url, ""), 403, "AccessDenied");
    }
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer", "user:test-signer@gcs-mimic.invalid")]}),
    )
    .success();
    for url in &urls {
        assert_eq!(send(&server, "GET", url, "").success().body, b"hello world");
    }
    let tampered = urls[0].replace("/object?", "/other?");
    error(
        &send(&server, "GET", &tampered, ""),
        403,
        "SignatureDoesNotMatch",
    );
}

#[test]
fn a_declared_built_in_signer_keeps_its_tokens_and_project_roles() {
    let config = TestDir::new();
    let data = TestDir::new();
    let mut identities = registry();
    identities["identities"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": test_signer::ID, "tokens": ["signer-token"],
            "project_roles": {"project-a": ["roles/storage.objectViewer"]}}));
    let path = common::process::write_identities(&config.0, &identities);
    let (server, ready) = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(ready["identities"], 7);
    assert_eq!(ready["test_signer"], "enabled");
    server
        .create_bucket(ADMIN, "project-a", "iam-bucket")
        .success();
    server
        .upload(ADMIN, "iam-bucket", "object", b"declared")
        .success();
    for url in [
        cli_signed(&server, "object"),
        v2(&server, "/iam-bucket/object"),
    ] {
        assert_eq!(send(&server, "GET", &url, "").success().body, b"declared");
    }
    let token = "Authorization: Bearer signer-token\r\n";
    assert_eq!(
        server
            .get(token, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .success()
            .body,
        b"declared"
    );
    server
        .upload(token, "iam-bucket", "denied", b"no")
        .forbidden();
}

#[test]
fn a_disabled_built_in_signer_is_an_unknown_signer() {
    let config = TestDir::new();
    let data = TestDir::new();
    let path = common::process::write_identities(&config.0, &registry());
    let (server, ready) = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            path.to_str().unwrap(),
            "--test-signer",
            "disabled",
        ],
    );
    assert_eq!(ready["test_signer"], "disabled");
    server
        .create_bucket(ADMIN, "project-a", "iam-bucket")
        .success();
    server
        .upload(ADMIN, "iam-bucket", "object", b"hello world")
        .success();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer", "user:test-signer@gcs-mimic.invalid")]}),
    )
    .success();
    for url in [
        v4(&server, "/iam-bucket/object"),
        v2(&server, "/iam-bucket/object"),
        cli_signed(&server, "object"),
    ] {
        error(&send(&server, "GET", &url, ""), 403, "InvalidAccessKeyId");
    }
    server
        .get(
            &jwt::bearer(test_signer::ID),
            "/storage/v1/b/iam-bucket/o/object?alt=media",
        )
        .invalid_credentials();
}

#[test]
fn a_self_signed_jwt_from_the_built_in_signer_authenticates_in_strict_mode() {
    let (_config, _data, server) = fixture();
    let path = "/storage/v1/b/iam-bucket/o/object?alt=media";
    let bearer = jwt::bearer(test_signer::ID);
    server.get(&bearer, path).forbidden();
    iam::set(
        &server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer", "user:test-signer@gcs-mimic.invalid")]}),
    )
    .success();
    assert_eq!(server.get(&bearer, path).success().body, b"hello world");
}
