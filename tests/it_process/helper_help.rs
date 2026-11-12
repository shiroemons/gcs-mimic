//! Helper-command help texts are documented commands and options, so their bytes are pinned as
//! `--help` is. `gcs-mimic sync` is left out: its options are outside the 1.0 contract until
//! directory synchronization is complete.
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
    ] {
        let output = common::server_command().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        assert_eq!(sha256_hex(&output.stdout), expected, "{args:?}");
    }
}
