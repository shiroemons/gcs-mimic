//! `gcs-mimic seed status|wait` against a running server: what a script reads and how the
//! command exits.

use crate::common::{self, TestDir};
use serde_json::{Value, json};
use std::{
    fs,
    process::{Output, Stdio},
};

common::process::declare_server!();

impl Server {
    fn start(data: &TestDir, options: &[&str]) -> Self {
        Self::launch(common::process::http_command(&data.0).args(options))
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.address)
    }
}

fn seed(args: &[&str]) -> Output {
    common::server_command()
        .arg("seed")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The JSON line the command printed.
fn printed(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("{error}: {output:?}"))
}

/// The one diagnostic the command explained its failure with.
fn diagnostic(output: &Output) -> String {
    let report: Value = serde_json::from_slice(&output.stderr)
        .unwrap_or_else(|error| panic!("{error}: {output:?}"));
    let [diagnostic] = report["diagnostics"].as_array().unwrap().as_slice() else {
        panic!("{report}");
    };
    assert_eq!(diagnostic["key"], "seed", "{report}");
    diagnostic["fix"].as_str().unwrap().to_owned()
}

/// A script waits until the server applied what it changed in the seed directory and reads the
/// report. An entry the pass leaves as it was fails only a wait that requires a clean pass, after
/// the report is printed. Without `--endpoint` the command finds the server as healthcheck does.
#[test]
fn a_script_waits_for_the_changed_seed_tree_and_reads_its_report() {
    let (data, source) = (TestDir::new(), TestDir::new());
    fs::create_dir(source.0.join("test-bucket")).unwrap();
    fs::write(source.0.join("test-bucket/kept"), b"kept").unwrap();
    let server = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
            "--seed-watch-interval-ms",
            "600000",
            "--max-object-bytes",
            "16",
        ],
    );
    let endpoint = server.endpoint();
    let status = seed(&["status", "--endpoint", &endpoint]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    assert_eq!(
        printed(&status),
        json!({"enabled": true, "intervalMs": 600000, "lastPass": null})
    );
    fs::write(source.0.join("test-bucket/added"), b"added").unwrap();
    fs::write(source.0.join("test-bucket/large"), [0; 17]).unwrap();
    let wait = seed(&["wait", "--endpoint", &endpoint, "--timeout", "30s"]);
    assert_eq!(wait.status.code(), Some(0), "{wait:?}");
    assert!(wait.stderr.is_empty(), "{wait:?}");
    let pass = printed(&wait);
    assert_eq!(
        (&pass["complete"], &pass["clean"], &pass["published"]),
        (&json!(true), &json!(false), &json!(1)),
        "{pass}"
    );
    assert_eq!(pass["problems"][0]["path"], "test-bucket/large", "{pass}");
    let clean = seed(&["wait", "--endpoint", &endpoint, "--require-clean"]);
    assert_eq!(clean.status.code(), Some(1), "{clean:?}");
    assert_eq!(printed(&clean)["problems"][0]["path"], "test-bucket/large");
    assert!(diagnostic(&clean).contains("--require-clean"), "{clean:?}");
    let status = printed(&seed(&["status", "--endpoint", &endpoint]));
    assert_eq!(status["lastPass"], printed(&clean), "{status}");
    fs::remove_file(source.0.join("test-bucket/large")).unwrap();
    let found = common::server_command()
        .args(["seed", "wait", "--require-clean"])
        .env("GCS_MIMIC_LISTEN", &server.address)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(found.status.code(), Some(0), "{found:?}");
    assert_eq!(printed(&found)["clean"], true, "{found:?}");
}

/// Each command fails with what to do next, and prints nothing as a report, when no pass can
/// answer it: no passes run, administration is off, no server listens or an option is wrong.
#[test]
fn the_seed_commands_fail_with_advice_when_no_pass_can_answer() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let seed_dir = source.0.to_str().unwrap();
    let stopped = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            seed_dir,
            "--seed-watch-interval-ms",
            "0",
        ],
    );
    let status = seed(&["status", "--endpoint", &stopped.endpoint()]);
    assert_eq!(status.status.code(), Some(0), "{status:?}");
    assert_eq!(
        printed(&status),
        json!({"enabled": false, "lastPass": null})
    );
    let wait = seed(&["wait", "--endpoint", &stopped.endpoint()]);
    assert_eq!(wait.status.code(), Some(1), "{wait:?}");
    assert!(wait.stdout.is_empty(), "{wait:?}");
    assert!(
        diagnostic(&wait).contains("answered HTTP 400: No pass replays seed_dir"),
        "{wait:?}"
    );
    let other = TestDir::new();
    let closed = Server::start(&other, &["--seed-dir", seed_dir]);
    let status = seed(&["status", "--endpoint", &closed.endpoint()]);
    assert_eq!(status.status.code(), Some(1), "{status:?}");
    assert!(
        diagnostic(&status).contains("--admin-enabled true"),
        "{status:?}"
    );
    let unreachable = seed(&["status", "--endpoint", "http://127.0.0.1:1"]);
    assert_eq!(unreachable.status.code(), Some(1), "{unreachable:?}");
    assert!(
        diagnostic(&unreachable)
            .starts_with("Cannot use the server at http://127.0.0.1:1: the connection failed"),
        "{unreachable:?}"
    );
    let usage = seed(&["wait", "--timeout", "11m"]);
    assert_eq!(usage.status.code(), Some(1), "{usage:?}");
    assert!(
        diagnostic(&usage).starts_with("Invalid seed command: --timeout must be 1ms through 10m"),
        "{usage:?}"
    );
}
