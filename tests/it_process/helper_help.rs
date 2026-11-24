//! Helper-command help texts are documented commands and options, so their bytes are pinned as
//! `--help` is.
use crate::common;

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn helper_help_texts_keep_their_exact_bytes() {
    for (args, expected) in [
        (
            ["url", "public", "--help"].as_slice(),
            "81cddd4bacb5ec6114102df52f375f4cc4acfeb1a3efd2b3d74990a1c6b1f577",
        ),
        (
            &["url", "sign", "--help"],
            "d26814250a6ccc4f37219ce3198de9d7604f02d9337920e22e32f6d13365b0f3",
        ),
        (
            &["test-signer", "--help"],
            "be07be46906f8de09115e250bddce7a39867b8641662f0ffa0e9d10fa29eff16",
        ),
        (
            &["import", "fake-gcs-server", "--help"],
            "47f0bfba4486d6e61bcfb7548f7d31a11c8dd9affb6639ef2e62ca739e1cd0a2",
        ),
        (
            &["buckets", "new", "--help"],
            "72077b230b83ae74854ed79bd4a0bbdf8f25f36ade44029004e5904247ba2d27",
        ),
    ] {
        let output = common::server_command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(sha256_hex(&output.stdout), expected, "{args:?}");
    }
}

/// `gcs-mimic sync` went with directory synchronization. A script that still runs it, even for
/// its help, stops before any option is read and learns that `--seed-dir` reads a folder.
#[test]
fn the_removed_sync_command_fails_with_its_replacement() {
    for args in [
        ["sync"].as_slice(),
        &["sync", "--help"],
        &["sync", "status", "--endpoint", "http://127.0.0.1:1"],
    ] {
        let output = common::server_command()
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let report = serde_json::from_str::<serde_json::Value>(stderr.trim()).unwrap();
        let diagnostic = &report["diagnostics"][0];
        assert_eq!(diagnostic["key"], "command", "{args:?}");
        assert_eq!(
            diagnostic["fix"],
            "gcs-mimic sync was removed with directory synchronization. seed_dir reads a folder without writing to it; start the server with --seed-dir DIRECTORY instead."
        );
    }
}
