use super::*;

const STATUS: &str = "/_internal/seed";
const WAIT: &str = "/_internal/seed/wait";

/// A server whose passes run only when a wait starts one: its interval is ten minutes.
fn watched(data: &TestDir, source: &TestDir) -> Server {
    std::fs::create_dir(source.0.join("test-bucket")).unwrap();
    std::fs::write(source.0.join("test-bucket/value"), b"seed").unwrap();
    Server::start(
        data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            source.0.to_str().unwrap(),
            "--seed-watch-interval-ms",
            "600000",
        ],
    )
}

/// A wait returns once the tree it found is applied, including the deletion of a removed file
/// that takes a second proof, and the status then reports that pass.
#[test]
fn a_wait_applies_the_changed_tree_and_the_status_reports_its_pass() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let server = watched(&data, &source);
    assert_eq!(
        server.request("GET", STATUS, "", b"").json_ok(),
        json!({"enabled": true, "intervalMs": 600000, "lastPass": null})
    );
    std::fs::write(source.0.join("test-bucket/added"), b"added").unwrap();
    std::fs::remove_file(source.0.join("test-bucket/value")).unwrap();
    let pass = server.request("POST", WAIT, "", b"").json_ok();
    assert_eq!(
        (
            &pass["complete"],
            &pass["clean"],
            &pass["deleted"],
            &pass["pendingDeletions"]
        ),
        (&json!(true), &json!(true), &json!(1), &json!(0)),
        "{pass}"
    );
    assert_eq!(pass["problems"], json!([]), "{pass}");
    server.request("GET", OBJECT, "", b"").expect(404);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/added?alt=media",
                "",
                b""
            )
            .body,
        b"added"
    );
    let status = server.request("GET", STATUS, "", b"").json_ok();
    assert_eq!(status["lastPass"], pass, "{status}");
}

#[test]
fn the_seed_routes_take_only_their_method_and_a_bounded_wait() {
    let (data, source) = (TestDir::new(), TestDir::new());
    let server = watched(&data, &source);
    server.request("POST", STATUS, "", b"").expect(405);
    server.request("GET", WAIT, "", b"").expect(405);
    server
        .request("GET", &format!("{STATUS}?fields=enabled"), "", b"")
        .json_reason(400, "invalidArgument");
    for query in ["timeoutMs=0", "timeoutMs=600001", "after=1"] {
        server
            .request("POST", &format!("{WAIT}?{query}"), "", b"")
            .json_reason(400, "invalidArgument");
    }
    server
        .request("POST", WAIT, "", b"{}")
        .json_reason(400, "invalidArgument");
    let pass = server
        .request("POST", &format!("{WAIT}?timeoutMs=30000"), "", b"")
        .json_ok();
    assert_eq!(pass["clean"], true, "{pass}");
}

/// Without passes the status says so and a wait fails at once; without local administration
/// neither route exists.
#[test]
fn the_seed_routes_without_passes_or_administration() {
    let (data, source) = (TestDir::new(), TestDir::new());
    std::fs::create_dir(source.0.join("test-bucket")).unwrap();
    let seed = source.0.to_str().unwrap();
    let server = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--seed-dir",
            seed,
            "--seed-watch-interval-ms",
            "0",
        ],
    );
    assert_eq!(
        server.request("GET", STATUS, "", b"").json_ok(),
        json!({"enabled": false, "lastPass": null})
    );
    let refused = server.request("POST", WAIT, "", b"");
    refused.json_reason(400, "invalid");
    assert!(!refused.headers.contains_key("retry-after"));
    drop(server);
    let server = Server::start(&data, &["--seed-dir", seed]);
    server.request("GET", STATUS, "", b"").expect(404);
    server.request("POST", WAIT, "", b"").expect(404);
}
