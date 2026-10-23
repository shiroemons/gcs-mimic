use gcs_mimic::{
    auth::{Auth, Registry},
    clock::Clock,
};
use hyper::{HeaderMap, Method, Uri};
use std::{
    process::{Command, Output},
    time::SystemTime,
};

const DATE: &str = "20261023T120000Z";
struct Fixed;
impl Clock for Fixed {
    fn now(&self) -> SystemTime {
        gcs_mimic::auth::signature::parse_timestamp(DATE).unwrap()
    }
}

fn run(extra: &[&str]) -> Output {
    run_options(&options(), extra)
}

fn options() -> Vec<&'static str> {
    vec![
        "url",
        "sign",
        "--base-url",
        "https://localhost:4443",
        "--bucket",
        "example-bucket",
        "--object",
        "é/a b",
        "--method",
        "GET",
        "--expires",
        "15m",
        "--signer",
        "local-viewer@example.invalid",
        "--private-key",
        "fixtures/signing/test-only-private.pem",
        "--timestamp",
        DATE,
    ]
}

fn run_options(options: &[&str], extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(options)
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn signing_cli_validates_duration_methods_timestamps_and_private_key_inputs() {
    for (duration, seconds) in [
        ("1", 1),
        ("2s", 2),
        ("15m", 900),
        ("2h", 7200),
        ("7d", 604800),
    ] {
        let mut args = options();
        args[11] = duration;
        let output = run_options(&args, &[]);
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("X-Goog-Expires={seconds}&"))
        );
    }
    for duration in [
        "0",
        "8d",
        "604801",
        "-1",
        "+1",
        "1.5m",
        "1w",
        "18446744073709551615d",
    ] {
        let mut args = options();
        args[11] = duration;
        let output = run_options(&args, &[]);
        assert!(!output.status.success(), "{duration}");
        assert!(output.stdout.is_empty());
    }
    for method in ["GET", "HEAD", "PUT"] {
        let mut args = options();
        args[9] = method;
        assert!(run_options(&args, &[]).status.success());
    }
    for (index, value) in [
        (9, "DELETE"),
        (17, "20260229T000000Z"),
        (15, "fixtures/signing/test-only-public.pem"),
        (15, "fixtures/signing/absent.pem"),
    ] {
        let mut args = options();
        args[index] = value;
        assert!(!run_options(&args, &[]).status.success());
    }
    for (base, expected) in [
        ("http://localhost:80", "http://localhost/example-bucket/"),
        ("https://localhost:443", "https://localhost/example-bucket/"),
    ] {
        let mut args = options();
        args[3] = base;
        let output = run_options(&args, &[]);
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).starts_with(expected));
    }
}

#[test]
fn signing_cli_outputs_a_verified_final_host_url_and_repeated_headers() {
    let output = run(&[
        "--header",
        "x-reviewer: jane doe",
        "--header",
        "x-reviewer:john",
        "--query",
        "response-content-disposition=attachment; filename*=UTF-8''%C3%A9%20%2B%20100%25.csv",
        "--query",
        "response-content-type=text/csv",
    ]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    assert!(text.starts_with("https://localhost:4443/example-bucket/%C3%A9/a%20b?"));
    assert!(text.contains("X-Goog-Expires=900"));
    assert!(text.contains("response-content-type=text%2Fcsv"));
    assert!(text.contains("filename%2A%3DUTF-8%27%27%25C3%25A9%2520%252B%2520100%2525.csv"));
    let uri: Uri = text.trim().parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("host", "localhost:4443".parse().unwrap());
    headers.insert("x-reviewer", "jane doe,john".parse().unwrap());
    let auth =
        Auth::strict(Registry::load(std::path::Path::new("config/identities.json")).unwrap());
    let caller = auth
        .authenticate_signed(&Method::GET, &uri, &headers, &Fixed)
        .unwrap()
        .unwrap();
    assert_eq!(
        caller.principal().unwrap().id(),
        "local-viewer@example.invalid"
    );
}

#[test]
fn signing_cli_rejects_unknown_duplicate_unpaired_and_forbidden_header_options() {
    for extra in [
        &["--unknown", "x"][..],
        &["--bucket", "other"],
        &["--header"],
        &["--header", "no-colon"],
        &["--header", "host:elsewhere"],
        &["--header", "Authorization:Bearer x"],
        &["--header", "bad name:x"],
        &["--query", "missing-equals"],
        &["--query", "=empty-name"],
        &["--query", "a=one", "--query", "a=two"],
        &["--query", "X-Goog-Date=20261023T120000Z"],
    ] {
        let output = run(extra);
        assert!(!output.status.success(), "{extra:?}");
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }
    let help = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(["url", "sign", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("GET|HEAD|PUT"));
    let missing = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(["url", "sign"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("missing required option"));
}
