use crate::common;

use common::process::{Server, http_command};
use std::process::{Command, Output, Stdio};

fn run(args: &[&str]) -> Output {
    // A closed stdin is no terminal, so these runs never wait for an answer.
    Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn diagnostic(output: &Output) -> serde_json::Value {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let line = String::from_utf8_lossy(&output.stderr);
    serde_json::from_str::<serde_json::Value>(line.trim()).unwrap()["diagnostics"][0].clone()
}

const SETTINGS: &[&str] = &[
    "buckets",
    "new",
    "--name",
    "app-uploads",
    "--versioning",
    "--label",
    "team=storage",
    "--cors-origin",
    "http://localhost:3000",
    "--cors-method",
    "GET",
    "--public-read",
    "--name",
    "app-exports",
];

/// The written document starts a server that creates exactly the buckets it names, from a file
/// and from the environment line alike.
#[test]
fn a_generated_document_initializes_its_buckets_at_startup() {
    let dir = common::TestDir::new();
    let path = dir.0.join("buckets.json");
    let mut args = SETTINGS.to_vec();
    args.extend(["--output", path.to_str().unwrap()]);
    let written = run(&args);
    assert!(written.status.success(), "{written:?}");
    assert!(written.stdout.is_empty(), "{written:?}");
    // The next-step advice is for a terminal; a script's stderr stays empty on success.
    assert!(written.stderr.is_empty(), "{written:?}");
    let server = Server::launch(
        http_command(&dir.0.join("file"))
            .arg("--buckets-file")
            .arg(&path),
    );
    assert_eq!(server.ready["buckets"]["created"], 2);
    drop(server);

    let mut env = SETTINGS.to_vec();
    env.extend(["--format", "env"]);
    let line = run(&env);
    assert!(line.status.success(), "{line:?}");
    let line = String::from_utf8(line.stdout).unwrap();
    let value = line
        .strip_prefix("GCS_MIMIC_BUCKETS='")
        .and_then(|rest| rest.strip_suffix("'\n"))
        .unwrap();
    let server = Server::launch(http_command(&dir.0.join("env")).env("GCS_MIMIC_BUCKETS", value));
    assert_eq!(server.ready["buckets"]["created"], 2);
    let bucket =
        common::effects::request(&server.address, "GET", "/storage/v1/b/app-uploads", "", b"")
            .json_ok();
    assert_eq!(bucket["versioning"]["enabled"], true);
    assert_eq!(bucket["labels"]["team"], "storage");
    drop(server);

    // Buckets without settings come out as their names, which startup creates as it would the
    // document.
    let names = run(&[
        "buckets",
        "new",
        "--name",
        "app-uploads",
        "--name",
        "app-exports",
        "--format",
        "env",
    ]);
    assert!(names.status.success(), "{names:?}");
    let names = String::from_utf8(names.stdout).unwrap();
    let value = names
        .strip_prefix("GCS_MIMIC_BUCKETS=")
        .and_then(|rest| rest.strip_suffix('\n'))
        .unwrap();
    assert_eq!(value, "app-uploads,app-exports");
    let server = Server::launch(http_command(&dir.0.join("names")).env("GCS_MIMIC_BUCKETS", value));
    assert_eq!(server.ready["buckets"]["created"], 2);
}

/// Questions need a terminal; without one and without --name nothing is asked or written.
#[test]
fn without_a_bucket_or_a_terminal_it_fails_before_asking() {
    let diagnostic = diagnostic(&run(&["buckets", "new"]));
    assert_eq!(diagnostic["key"], "--name");
    assert!(
        diagnostic["fix"].as_str().unwrap().contains("--name NAME"),
        "{diagnostic}"
    );
}

#[test]
fn an_existing_output_file_is_kept() {
    let dir = common::TestDir::new();
    let path = dir.0.join("buckets.json");
    std::fs::write(&path, b"kept").unwrap();
    let diagnostic = diagnostic(&run(&[
        "buckets",
        "new",
        "--name",
        "app-uploads",
        "--output",
        path.to_str().unwrap(),
    ]));
    assert_eq!(diagnostic["key"], "--output");
    assert_eq!(std::fs::read(&path).unwrap(), b"kept");
}

/// A document startup would refuse is not written, and the diagnostic keeps the entry path.
#[test]
fn a_document_startup_would_refuse_is_not_written() {
    let refused = diagnostic(&run(&[
        "buckets",
        "new",
        "--name",
        "app-uploads",
        "--location",
        "MOON-CENTRAL1",
    ]));
    assert_eq!(refused["key"], "buckets[0].location");
    assert_eq!(diagnostic(&run(&["buckets", "list"]))["key"], "command");
}

/// Startup accepts a CORS rule without a method, which matches no request, so the command
/// refuses it and writes nothing rather than report success for CORS that never applies.
#[test]
fn a_cors_rule_that_matches_no_request_is_not_written() {
    let dir = common::TestDir::new();
    let path = dir.0.join("buckets.json");
    let refused = diagnostic(&run(&[
        "buckets",
        "new",
        "--name",
        "app-uploads",
        "--cors-origin",
        "http://localhost:3000",
        "--output",
        path.to_str().unwrap(),
    ]));
    assert_eq!(refused["key"], "--cors-method");
    assert!(!path.exists());
}
