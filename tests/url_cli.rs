use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(["url", "public"])
        .args(args)
        .output()
        .unwrap()
}

fn assert_success(output: &Output, expected: &str) {
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{expected}\n")
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn assert_failure(output: &Output, message: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(message), "{stderr}");
}

#[test]
fn prints_one_encoded_url_for_options_in_any_order() {
    let expected = "http://127.0.0.1:4443/prefix/my-bucket/dir/a%20b%23%3F%25%2B%C3%A9~.txt";
    let base = ["--base-url", "http://127.0.0.1:4443/prefix/"];
    let bucket = ["--bucket", "my-bucket"];
    let object = ["--object", "dir/a b#?%+é~.txt"];
    for order in [
        [base, bucket, object],
        [object, base, bucket],
        [bucket, object, base],
    ] {
        assert_success(&run(&order.concat()), expected);
    }
    assert_success(
        &run(&[
            "--object",
            "a/../b",
            "--bucket",
            "my-bucket",
            "--base-url",
            "https://storage.example.invalid",
        ]),
        "https://storage.example.invalid/my-bucket/a%2F..%2Fb",
    );
}

#[test]
fn prints_virtual_host_url_with_dotted_bucket_and_explicit_port() {
    assert_success(
        &run(&[
            "--base-url",
            "http://storage.invalid:4443",
            "--bucket",
            "dotted.bucket",
            "--object",
            "é//a b",
            "--style",
            "virtual-hosted",
        ]),
        "http://dotted.bucket.storage.invalid:4443/%C3%A9//a%20b",
    );
    assert_failure(
        &run(&[
            "--base-url",
            "http://storage.invalid",
            "--bucket",
            "bucket",
            "--object",
            "o",
            "--style",
            "invalid",
        ]),
        "style must be",
    );
}

#[test]
fn prints_usage_for_help() {
    let output = run(&["--help"]);
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.starts_with("Usage: gcs-mimic url public --base-url URL"));
    assert!(output.stderr.is_empty());
    let general = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&general.stdout)
            .contains("gcs-mimic url public --base-url URL --bucket NAME --object NAME")
    );
}

#[test]
fn rejects_missing_duplicate_unknown_and_unpaired_options() {
    let base = "http://localhost:4443";
    assert_failure(
        &run(&["--bucket", "my-bucket", "--object", "o"]),
        "missing required option: --base-url",
    );
    assert_failure(
        &run(&["--base-url", base, "--object", "o"]),
        "missing required option: --bucket",
    );
    assert_failure(
        &run(&["--base-url", base, "--bucket", "my-bucket"]),
        "missing required option: --object",
    );
    assert_failure(&run(&[]), "missing required option");
    assert_failure(
        &run(&[
            "--base-url",
            base,
            "--bucket",
            "my-bucket",
            "--object",
            "o",
            "--bucket",
            "other",
        ]),
        "duplicate option: --bucket",
    );
    assert_failure(
        &run(&[
            "--base-url",
            base,
            "--bucket",
            "my-bucket",
            "--object",
            "o",
            "--project",
            "p",
        ]),
        "unknown url public option: --project",
    );
    assert_failure(
        &run(&["--base-url", base, "--bucket", "my-bucket", "--object"]),
        "--name VALUE pairs",
    );
    assert_failure(
        &run(&[
            "--base-url",
            base,
            "--bucket",
            "my-bucket",
            "--object",
            "o",
            "--help",
        ]),
        "--name VALUE pairs",
    );
}

#[test]
fn rejects_invalid_inputs_without_stdout() {
    let valid = [
        "--base-url",
        "http://localhost:4443",
        "--bucket",
        "my-bucket",
        "--object",
        "o",
    ];
    for (index, value, message) in [
        (1, "ftp://localhost", "base URL must be"),
        (1, "http://user@localhost", "base URL must be"),
        (1, "http://localhost?x=1", "base URL must be"),
        (1, "http://localhost/#f", "base URL must be"),
        (1, "relative", "base URL must be"),
        (3, "Invalid_Bucket", "bucket: Invalid bucket name"),
        (5, "..", "object: Invalid object name"),
        (5, "", "object: Invalid object name"),
        (5, "a\nb", "object: Invalid object name"),
    ] {
        let mut args = valid;
        args[index] = value;
        assert_failure(&run(&args), message);
    }
}

#[test]
fn rejects_reserved_api_collisions_without_stdout() {
    for (bucket, object) in [
        ("storage", "v1"),
        ("storage", "v1/x"),
        ("upload", "storage/v1"),
        ("upload", "storage/v1/x"),
        ("download", "storage/v1"),
        ("download", "storage/v1/x"),
        ("batch", "storage/v1"),
        ("batch", "storage/v1/x"),
    ] {
        assert_failure(
            &run(&[
                "--base-url",
                "http://localhost:4443",
                "--bucket",
                bucket,
                "--object",
                object,
            ]),
            "object URL collides with a reserved API namespace",
        );
    }
    for (bucket, object) in [
        ("batch", "x"),
        ("batch", "storage/v1-other/x"),
        ("storage", "v1-other/x"),
        ("upload", "storage/v1-other/x"),
        ("download", "storage/v1-other/x"),
        ("storage", "v1/../x"),
    ] {
        assert_success(
            &run(&[
                "--base-url",
                "http://localhost:4443",
                "--bucket",
                bucket,
                "--object",
                object,
            ]),
            &format!(
                "http://localhost:4443/{bucket}/{}",
                gcs_mimic::url::encode_object_name(object)
            ),
        );
    }
}

#[test]
fn ignores_config_file_environment_and_data_directory() {
    let missing =
        std::env::temp_dir().join(format!("gcs-mimic-url-missing-{}", std::process::id()));
    assert!(!missing.exists());
    let output = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args([
            "url",
            "public",
            "--base-url",
            "http://localhost:4443",
            "--bucket",
            "my-bucket",
            "--object",
            "o",
        ])
        .env("GCS_MIMIC_CONFIG", missing.join("config.json"))
        .env("GCS_MIMIC_DATA_DIR", missing.join("data"))
        .env("GCS_MIMIC_API_BASE_URL", "not a url")
        .env("GCS_MIMIC_UNKNOWN_SETTING", "x")
        .output()
        .unwrap();
    assert_success(&output, "http://localhost:4443/my-bucket/o");
    assert!(!missing.exists());
}
