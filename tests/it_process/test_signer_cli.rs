use crate::common;

use common::process::{Server, http_command};
use gcs_mimic::auth::signature::{SignUrlRequest, sign_url};
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(args)
        .output()
        .unwrap()
}

fn credentials() -> serde_json::Value {
    let output = run(&["test-signer", "credentials"]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn credentials_print_a_service_account_key_file_for_the_built_in_signer() {
    let credentials = credentials();
    assert_eq!(credentials["type"], "service_account");
    assert_eq!(credentials["client_email"], "test-signer@gcs-mimic.invalid");
    assert_eq!(credentials["project_id"], "local-project");
    assert_eq!(credentials["universe_domain"], "gcs-emulator.invalid");
    assert!(
        credentials["private_key"]
            .as_str()
            .unwrap()
            .starts_with("-----BEGIN PRIVATE KEY-----\n")
    );
    let help = run(&["test-signer", "--help"]);
    assert!(help.status.success(), "{help:?}");
    assert!(String::from_utf8_lossy(&help.stdout).contains("test-signer credentials"));
}

/// The printed key is the one the server embeds: a URL it signs verifies without any file.
#[test]
fn a_url_signed_with_the_printed_key_verifies_against_the_embedded_public_key() {
    let dir = common::TestDir::new();
    let server = Server::launch(http_command(&dir.0).args(["--signed-urls", "verify"]));
    assert_eq!(server.ready["test_signer"], "enabled");
    common::effects::request(
        &server.address,
        "POST",
        "/storage/v1/b?project=local-project",
        "",
        br#"{"name":"signer-bucket"}"#,
    )
    .json_ok();
    common::effects::request(
        &server.address,
        "POST",
        "/upload/storage/v1/b/signer-bucket/o?uploadType=media&name=object",
        "",
        b"signed bytes",
    )
    .json_ok();
    let credentials = credentials();
    let url = sign_url(SignUrlRequest {
        url: "http://localhost/signer-bucket/object",
        method: &hyper::Method::GET,
        signer: credentials["client_email"].as_str().unwrap(),
        private_key: credentials["private_key"].as_str().unwrap().as_bytes(),
        expires: 60,
        now: std::time::SystemTime::now(),
        headers: hyper::HeaderMap::new(),
    })
    .unwrap();
    let get = |path: &str| common::effects::request(&server.address, "GET", path, "", b"");
    let path = url.strip_prefix("http://localhost").unwrap();
    let reply = get(path);
    reply.expect(200);
    assert_eq!(reply.body, b"signed bytes");
    // A changed signature fails, so the success above came from verifying the printed key.
    let (unsigned, signature) = path.rsplit_once('=').unwrap();
    let flipped = if signature.starts_with('0') { "1" } else { "0" };
    let tampered = format!("{unsigned}={flipped}{}", &signature[1..]);
    assert_eq!(get(&tampered).status, 403);
}

#[test]
fn unknown_test_signer_arguments_are_rejected() {
    for args in [
        &["test-signer"][..],
        &["test-signer", "key"],
        &["test-signer", "credentials", "extra"],
        &["test-signer", "credentials", "--format", "pem"],
    ] {
        let output = run(args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
}
