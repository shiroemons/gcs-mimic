#[allow(dead_code)]
mod common;

use common::TestDir;
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(kind: &str, config: Option<&Path>, extra: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"));
    command
        .env_clear()
        .args(["url", kind, "--bucket", "my-bucket", "--object", "a b"]);
    // Invalid implicit inputs prove these helpers never invoke server configuration loading.
    command
        .env("GCS_MIMIC_CONFIG", "missing-config.json")
        .env("GCS_MIMIC_PUBLIC_BASE_URL", "http://implicit.invalid")
        .env("GCS_MIMIC_AUTH_MODE", "strict");
    if kind == "sign" {
        command.args([
            "--method",
            "GET",
            "--expires",
            "60",
            "--signer",
            "admin@example.invalid",
            "--private-key",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fixtures/signing/test-only-private.pem"
            ),
            "--timestamp",
            "20261024T120000Z",
        ]);
    }
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    command.args(extra).output().unwrap()
}

fn success(output: Output, base: &str, kind: &str) {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let url = String::from_utf8(output.stdout).unwrap();
    let expected = format!("{base}/my-bucket/a%20b");
    if kind == "public" {
        assert_eq!(url.trim_end(), expected);
    } else {
        assert!(url.starts_with(&format!("{expected}?")), "{url}");
        assert!(url.contains("X-Goog-Signature="), "{url}");
    }
}

fn failure(output: Output, message: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(message),
        "{output:?}"
    );
}

#[test]
fn helpers_read_only_explicit_public_origin_and_keep_base_overrides() {
    let dir = TestDir::new();
    let config = dir.0.join("config.json");
    for kind in ["public", "sign"] {
        failure(run(kind, None, &[]), "--base-url");
        for base in [
            "http://localhost:4443",
            "http://gcs:4443",
            "https://browser.example.invalid",
        ] {
            std::fs::write(&config, serde_json::json!({
                "public_base_url": format!("{base}/"), "auth_mode": "strict", "tls_cert": "missing.pem",
                "identity_file": "missing.json", "data_dir": dir.0.join("untouched").to_str().unwrap(),
                "unrecognized_key": 12, "max_connections": 0,
            }).to_string()).unwrap();
            success(run(kind, Some(&config), &[]), base, kind);
            success(
                run(
                    kind,
                    Some(&config),
                    &["--base-url", "http://override.invalid/prefix"],
                ),
                "http://override.invalid/prefix",
                kind,
            );
            assert!(!dir.0.join("untouched").exists());
        }
        std::fs::write(
            &config,
            r#"{"public_base_url":"https://Storage.Invalid:443///"}"#,
        )
        .unwrap();
        success(
            run(kind, Some(&config), &[]),
            "https://storage.invalid",
            kind,
        );
        assert_eq!(
            run(kind, Some(&config), &[]).stdout,
            run(kind, None, &["--base-url", "https://storage.invalid"]).stdout
        );
        std::fs::write(&config, r#"{"unknown":42}"#).unwrap();
        success(
            run(
                kind,
                Some(&config),
                &["--base-url", "http://explicit.invalid"],
            ),
            "http://explicit.invalid",
            kind,
        );
        failure(run(kind, Some(&config), &[]), "public_base_url");
    }
}

#[test]
fn helpers_validate_explicit_file_even_when_the_base_is_overridden() {
    let dir = TestDir::new();
    let config = dir.0.join("config.json");
    for kind in ["public", "sign"] {
        failure(
            run(
                kind,
                Some(&dir.0.join("missing.json")),
                &["--base-url", "http://explicit.invalid"],
            ),
            "Cannot read config",
        );
        assert!(
            !String::from_utf8_lossy(&run(kind, Some(&dir.0.join("missing.json")), &[]).stderr)
                .contains("GCS_MIMIC_CONFIG")
        );
        std::fs::write(&config, r#"{"named_bad_field":true}"#).unwrap();
        failure(run(kind, Some(&config), &[]), "named_bad_field");
        let output = run(kind, Some(&config), &[]);
        let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["diagnostics"][0]["key"], "named_bad_field");
        assert_eq!(diagnostic["diagnostics"][0]["source"], "config");
        let output = run(kind, Some(&dir.0.join("missing.json")), &[]);
        let diagnostic: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["diagnostics"][0]["key"], "--config");
        for content in [
            "[]",
            "null",
            "{",
            r#"{"unknown":true}"#,
            r#"{"unknown":{}}"#,
            r#"{"unknown":-1}"#,
            r#"{"unknown":1.5}"#,
            r#"{"public_base_url":"http://storage.invalid/prefix"}"#,
            r#"{"public_base_url":"http://user@storage.invalid"}"#,
            r#"{"public_base_url":42}"#,
        ] {
            std::fs::write(&config, content).unwrap();
            let output = run(
                kind,
                Some(&config),
                &["--base-url", "http://explicit.invalid"],
            );
            assert!(!output.status.success(), "{kind}: {content}: {output:?}");
            assert!(output.stdout.is_empty());
        }
        std::fs::write(
            &config,
            format!("{{\"unused\":\"{}\"}}", "a".repeat(65_536)),
        )
        .unwrap();
        failure(
            run(
                kind,
                Some(&config),
                &["--base-url", "http://explicit.invalid"],
            ),
            "64 KiB",
        );
        std::fs::write(&config, r#"{"public_base_url":"http://127.0.0.1:4443"}"#).unwrap();
        failure(
            run(kind, Some(&config), &["--style", "virtual-hosted"]),
            "DNS name",
        );
        failure(
            run(kind, Some(&config), &["--config", config.to_str().unwrap()]),
            "duplicate option: --config",
        );
    }
}
