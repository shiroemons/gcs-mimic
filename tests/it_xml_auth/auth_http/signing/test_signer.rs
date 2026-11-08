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
        test_signer::PRIVATE_KEY_PEM.as_bytes(),
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

/// The identity file entry the declared cases add for the signer's ID.
fn declared_signer() -> Value {
    json!({"id": test_signer::ID, "tokens": ["signer-token"],
        "project_roles": {"project-a": ["roles/storage.objectViewer"]}})
}

/// A strict server whose `iam-bucket` holds `object`, optionally declaring the signer's ID.
fn strict(declared: bool, options: &[&str], stderr: Stdio) -> (TestDir, TestDir, Server, Value) {
    let config = TestDir::new();
    let data = TestDir::new();
    let mut identities = registry();
    if declared {
        identities["identities"]
            .as_array_mut()
            .unwrap()
            .push(declared_signer());
    }
    let path = common::process::write_identities(&config.0, &identities);
    let mut args = vec![
        "--auth-mode",
        "strict",
        "--identity-file",
        path.to_str().unwrap(),
    ];
    args.extend_from_slice(options);
    let (server, ready) = Server::start_with_stderr(&data, &args, stderr);
    server
        .create_bucket(ADMIN, "project-a", "iam-bucket")
        .success();
    server
        .upload(ADMIN, "iam-bucket", "object", b"hello world")
        .success();
    (config, data, server, ready)
}

fn grant_signer(server: &Server) {
    iam::set(
        server,
        ADMIN,
        &json!({"bindings":[iam::binding("roles/storage.objectViewer", "user:test-signer@gcs-mimic.invalid")]}),
    )
    .success();
}

#[test]
fn an_enabled_undeclared_signer_verifies_v4_and_v2_needs_a_grant_and_warns() {
    let (_config, _data, mut server, ready) =
        strict(false, &["--test-signer", "enabled"], Stdio::piped());
    assert_eq!(ready["test_signer"], "enabled");
    assert_eq!(ready["identities"], 6);
    let urls = [
        v4(&server, "/iam-bucket/object"),
        v2(&server, "/iam-bucket/object"),
        cli_signed(&server, "object"),
    ];
    for url in &urls {
        error(&send(&server, "GET", url, ""), 403, "AccessDenied");
    }
    grant_signer(&server);
    for url in &urls {
        assert_eq!(send(&server, "GET", url, "").success().body, b"hello world");
    }
    let tampered = urls[0].replace("/object?", "/other?");
    error(
        &send(&server, "GET", &tampered, ""),
        403,
        "SignatureDoesNotMatch",
    );
    let warnings: Vec<_> = stderr_events(&mut server)
        .into_iter()
        .filter(|event| event["event"] == "diagnostics" && event["level"] == "warn")
        .flat_map(|event| event["diagnostics"].as_array().unwrap().clone())
        .filter(|diagnostic| diagnostic["key"] == "--test-signer")
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let message = warnings[0]["fix"].as_str().unwrap();
    for expected in [
        "test-signer@gcs-mimic.invalid is published",
        "anyone can authenticate",
        "allAuthenticatedUsers",
        "only for local tests",
    ] {
        assert!(message.contains(expected), "{message}");
    }
}

#[test]
fn an_enabled_declared_signer_keeps_its_tokens_and_project_roles() {
    let (_config, _data, server, ready) =
        strict(true, &["--test-signer", "enabled"], Stdio::null());
    assert_eq!(ready["identities"], 7);
    assert_eq!(ready["test_signer"], "enabled");
    for url in [
        cli_signed(&server, "object"),
        v2(&server, "/iam-bucket/object"),
    ] {
        assert_eq!(
            send(&server, "GET", &url, "").success().body,
            b"hello world"
        );
    }
    let token = "Authorization: Bearer signer-token\r\n";
    assert_eq!(
        server
            .get(token, "/storage/v1/b/iam-bucket/o/object?alt=media")
            .success()
            .body,
        b"hello world"
    );
    server
        .upload(token, "iam-bucket", "denied", b"no")
        .forbidden();
}

/// Strict mode leaves the signer out when nothing asks for it, and `test_signer=disabled` keeps
/// an entry for its ID an ordinary identity without the published key.
#[test]
fn strict_mode_has_no_built_in_signer_by_default_or_when_disabled() {
    let disabled = &["--test-signer", "disabled"][..];
    for (declared, options) in [(false, &[][..]), (false, disabled), (true, disabled)] {
        let (_config, _data, server, ready) = strict(declared, options, Stdio::inherit());
        assert_eq!(ready["test_signer"], "disabled");
        assert_eq!(ready["identities"], if declared { 7 } else { 6 });
        grant_signer(&server);
        for url in [
            v4(&server, "/iam-bucket/object"),
            v2(&server, "/iam-bucket/object"),
            cli_signed(&server, "object"),
        ] {
            let reply = send(&server, "GET", &url, "");
            error(&reply, 403, "InvalidAccessKeyId");
            // The answer names the setting rather than an unknown registered identity.
            assert!(
                    String::from_utf8_lossy(&reply.body).contains(
                        "test-signer@gcs-mimic.invalid is the built-in test signer, which is disabled; set test_signer=enabled."
                    ),
                    "{}",
                    String::from_utf8_lossy(&reply.body)
                );
        }
        let unknown = sign(
            &server,
            &Method::GET,
            "/iam-bucket/object",
            "unknown@example.invalid",
            SystemTime::now(),
            HeaderMap::new(),
        );
        let reply = send(&server, "GET", &unknown, "");
        error(&reply, 403, "InvalidAccessKeyId");
        assert!(
            String::from_utf8_lossy(&reply.body)
                .contains("The signing key or its registered identity is unknown or inactive.")
        );
        let path = "/storage/v1/b/iam-bucket/o/object?alt=media";
        server
            .get(&jwt::bearer(test_signer::ID), path)
            .invalid_credentials();
        if declared {
            let token = "Authorization: Bearer signer-token\r\n";
            assert_eq!(server.get(token, path).success().body, b"hello world");
        }
    }
}

/// An identity entry for the signer's ID asks for the signer, so strict mode enables it without
/// `test_signer` and warns at the setting that supplied the entry.
#[test]
fn a_declared_signer_enables_the_built_in_key_by_default_and_warns_at_its_setting() {
    let (_config, _data, mut server, ready) = strict(true, &[], Stdio::piped());
    assert_eq!(ready["test_signer"], "enabled");
    assert_eq!(ready["identities"], 7);
    // The entry's own project role authorizes what the published key signs.
    for url in [
        v4(&server, "/iam-bucket/object"),
        v2(&server, "/iam-bucket/object"),
        cli_signed(&server, "object"),
    ] {
        assert_eq!(
            send(&server, "GET", &url, "").success().body,
            b"hello world"
        );
    }
    let token = "Authorization: Bearer signer-token\r\n";
    server
        .upload(token, "iam-bucket", "denied", b"no")
        .forbidden();
    let warnings: Vec<_> = stderr_events(&mut server)
        .into_iter()
        .filter(|event| event["event"] == "diagnostics" && event["level"] == "warn")
        .flat_map(|event| event["diagnostics"].as_array().unwrap().clone())
        .filter(|diagnostic| {
            diagnostic["fix"]
                .as_str()
                .is_some_and(|fix| fix.contains("is published"))
        })
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["key"], "--identity-file");
    let message = warnings[0]["fix"].as_str().unwrap();
    for expected in [
        "identity_file declares test-signer@gcs-mimic.invalid",
        "anyone can authenticate",
        "allAuthenticatedUsers",
        "test_signer=disabled",
    ] {
        assert!(message.contains(expected), "{message}");
    }
}

/// Without an entry for its ID an enabled signer holds `roles/storage.objectUser` in the setup's
/// projects: signed reads and writes work there, while IAM and ACL changes stay out of reach of
/// the published key, and other projects still need a grant.
#[test]
fn an_undeclared_signer_reads_and_writes_the_setups_buckets_but_administers_nothing() {
    let config = TestDir::new();
    let data = TestDir::new();
    let path = common::process::write_identities(&config.0, &registry());
    let buckets =
        json!({"version":1,"project":"project-a","buckets":[{"name":"signer-bucket"}]}).to_string();
    let args = [
        "--auth-mode",
        "strict",
        "--identity-file",
        path.to_str().unwrap(),
        "--test-signer",
        "enabled",
        "--buckets",
        &buckets,
    ];
    let (mut server, ready) = Server::start_with_stderr(&data, &args, Stdio::piped());
    assert_eq!(ready["test_signer"], "enabled");
    let signed = |method: Method, path: &str| {
        sign(
            &server,
            &method,
            path,
            test_signer::ID,
            SystemTime::now(),
            HeaderMap::new(),
        )
    };
    send(
        &server,
        "PUT",
        &signed(Method::PUT, "/signer-bucket/signed.txt"),
        "",
    )
    .success();
    send(
        &server,
        "GET",
        &signed(Method::GET, "/signer-bucket/signed.txt"),
        "",
    )
    .success();
    send(
        &server,
        "PUT",
        &signed(Method::PUT, "/signer-bucket/signed.txt"),
        "",
    )
    .success();
    let json = "Content-Type: application/json\r\n";
    let bearer = format!("{}{json}", jwt::bearer(test_signer::ID));
    server
        .get(
            &jwt::bearer(test_signer::ID),
            "/storage/v1/b/signer-bucket/o",
        )
        .success();
    server
        .request(
            "PUT",
            "/storage/v1/b/signer-bucket/iam",
            &bearer,
            br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
        )
        .forbidden();
    // The role grants no ACL administration. An object the signer writes records it as OWNER,
    // which, as in Cloud Storage's fine-grained ACLs, lets it edit that object's own ACL only.
    server
        .upload(ADMIN, "signer-bucket", "admin.txt", b"admin bytes")
        .success();
    for path in [
        "/storage/v1/b/signer-bucket/acl",
        "/storage/v1/b/signer-bucket/o/admin.txt/acl",
    ] {
        server
            .request(
                "POST",
                path,
                &bearer,
                br#"{"entity":"allUsers","role":"READER"}"#,
            )
            .forbidden();
    }
    // The seeded iam-bucket is in project-a too; a project the setup does not name needs a grant.
    server
        .create_bucket(ADMIN, "project-b", "other-bucket")
        .success();
    error(
        &send(
            &server,
            "PUT",
            &signed(Method::PUT, "/other-bucket/denied.txt"),
            "",
        ),
        403,
        "AccessDenied",
    );
    let warnings: Vec<_> = stderr_events(&mut server)
        .into_iter()
        .filter(|event| event["event"] == "diagnostics" && event["level"] == "warn")
        .flat_map(|event| event["diagnostics"].as_array().unwrap().clone())
        .filter(|diagnostic| diagnostic["key"] == "--test-signer")
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let message = warnings[0]["fix"].as_str().unwrap();
    assert!(
        message.contains(
            "It holds roles/storage.objectUser in local-project, project-a, so it can read and write objects there."
        ),
        "{message}"
    );
}

#[test]
fn a_self_signed_jwt_from_an_enabled_built_in_signer_authenticates_in_strict_mode() {
    let (_config, _data, server, _ready) =
        strict(false, &["--test-signer", "enabled"], Stdio::null());
    let path = "/storage/v1/b/iam-bucket/o/object?alt=media";
    let bearer = jwt::bearer(test_signer::ID);
    server.get(&bearer, path).forbidden();
    grant_signer(&server);
    assert_eq!(server.get(&bearer, path).success().body, b"hello world");
}
