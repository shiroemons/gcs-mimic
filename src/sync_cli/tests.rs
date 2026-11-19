use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn parse(args: &[&str]) -> Result<Options, CliError> {
    Options::parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
}

fn parse_cmd(args: &[&str]) -> Result<Options, CliError> {
    let (command, options) = args.split_first().unwrap();
    let mut args = vec![*command, "--endpoint", "http://127.0.0.1:4443"];
    args.extend_from_slice(options);
    parse(&args)
}

/// The one diagnostic `args` is refused with.
fn refusal(args: &[&str]) -> String {
    match parse(args) {
        Err(CliError::Diagnostics(diagnostics)) => {
            let [error] = diagnostics.errors.as_slice() else {
                panic!("{args:?}: one error expected: {diagnostics:?}");
            };
            assert_eq!(error.key, "sync", "{args:?}");
            error.fix.clone()
        }
        Err(error) => panic!("{args:?}: refused without a diagnostic: {error:?}"),
        Ok(_) => panic!("{args:?}: accepted"),
    }
}

#[test]
fn parse_failures_keep_their_text_and_the_order_they_are_reported_in() {
    let endpoint = "http://127.0.0.1:4443";
    let revision = "0123456789abcdef0123456789abcdef";
    for (args, fix) in [
        (vec![], HELP),
        (
            vec!["status", "--endpoint"],
            "Use unique sync option/value pairs; see sync --help",
        ),
        (
            vec!["status", "--unknown", "x"],
            "Use unique sync option/value pairs; see sync --help",
        ),
        (
            vec!["status", "--endpoint", ""],
            "Use unique sync option/value pairs; see sync --help",
        ),
        (
            vec!["status", "--view", "other"],
            "--view must be deletions or findings",
        ),
        // Approval refuses --revision before it looks for an endpoint.
        (
            vec!["approve", "--revision", revision],
            "sync approve takes --deletion-set REVISION, not --revision",
        ),
        // An unknown operation is reported only after the shared options are read.
        (vec!["stop"], "Supply --endpoint http(s)://HOST:PORT"),
        (
            vec!["stop", "--endpoint", "http://127.0.0.1:1"],
            "Use sync status, findings, deletions, conflicts, rescan, approve, resolve or wait with their documented options",
        ),
        (
            vec!["stop", "--endpoint", endpoint, "--revision", "x"],
            "Use the exact lowercase inspected revision: 48 characters for findings, 32 for deletions/approval/conflicts",
        ),
        (
            vec!["status", "--endpoint", "http://local host:1"],
            "Invalid HTTP(S) endpoint origin",
        ),
        (
            vec!["status", "--endpoint", "http://localhost"],
            "Supply a bare HTTP(S) origin with an explicit nonzero port",
        ),
        // The origin is checked before the certificate that needs its scheme.
        (
            vec![
                "status",
                "--endpoint",
                "http://localhost",
                "--ca-cert",
                "ca",
            ],
            "Supply a bare HTTP(S) origin with an explicit nonzero port",
        ),
        (
            vec!["status", "--endpoint", endpoint, "--ca-cert", "ca"],
            "--ca-cert requires an HTTPS endpoint",
        ),
        (
            vec![
                "status",
                "--endpoint",
                endpoint,
                "--timeout",
                "1s",
                "--timeout-ms",
                "1000",
            ],
            "Supply only one timeout option",
        ),
        (
            vec!["status", "--endpoint", endpoint, "--timeout-ms", "0"],
            "--timeout-ms must be 1 through 60000",
        ),
        (
            vec!["status", "--endpoint", endpoint, "--revision", revision],
            "Use sync status, findings, deletions, conflicts, rescan, approve, resolve or wait with their documented options",
        ),
        (
            vec!["findings", "--endpoint", endpoint, "--revision", revision],
            "Use the exact lowercase inspected revision: 48 characters for findings, 32 for deletions/approval/conflicts",
        ),
        (
            vec!["deletions", "--endpoint", endpoint],
            "Pages require the matching --revision from status",
        ),
        (
            vec!["conflicts", "--endpoint", endpoint, "--limit", "0"],
            "--limit must be 1 through 256",
        ),
        (
            vec!["approve", "--endpoint", endpoint],
            "Approval requires --deletion-set REVISION from an inspected deletion set",
        ),
        (
            vec!["resolve", "--endpoint", endpoint],
            "Resolve requires the inspected --revision",
        ),
        (
            vec!["rescan", "--endpoint", endpoint, "--deletion-set", "x"],
            "This sync operation does not accept the remaining options; see sync --help",
        ),
        (
            vec!["rescan", "--endpoint", endpoint, "--view", "deletions"],
            "This sync operation does not accept the remaining options; see sync --help",
        ),
    ] {
        assert_eq!(refusal(&args), fix, "{args:?}");
    }
}

#[test]
fn timeout_durations_preserve_cli_units_bounds_and_leading_plus() {
    for (value, fix) in [
        ("0ms", Some("--timeout must be 1ms through 60s")),
        ("60001ms", Some("--timeout must be 1ms through 60s")),
        ("2m", Some("--timeout must be 1ms through 60s")),
        (
            "5x",
            Some("--timeout requires an integer with ms, s or m suffix"),
        ),
        ("+5s", None),
    ] {
        let result = parse_cmd(&["status", "--timeout", value]);
        if let Some(fix) = fix {
            assert!(result.err().unwrap().to_string().contains(fix), "{value}");
        } else {
            assert_eq!(result.unwrap().timeout, Duration::from_secs(5));
        }
    }
}

#[test]
fn rescan_default_leaves_time_for_quiet_and_honors_explicit_deadlines() {
    assert_eq!(
        parse_cmd(&["rescan"]).unwrap().timeout,
        Duration::from_secs(30)
    );
    assert_eq!(
        parse_cmd(&["status"]).unwrap().timeout,
        Duration::from_secs(10)
    );
    // A wait asks again until its own timeout, which leaves a first pass over a
    // local-development tree on a slow bind mount time to finish.
    assert_eq!(
        parse_cmd(&["wait"]).unwrap().timeout,
        Duration::from_secs(600)
    );
    assert_eq!(
        parse_cmd(&["rescan", "--timeout", "5s"]).unwrap().timeout,
        Duration::from_secs(5)
    );
    assert_eq!(
        parse_cmd(&["rescan", "--timeout-ms", "25"])
            .unwrap()
            .timeout,
        Duration::from_millis(25)
    );
    assert!(HELP.contains("Wait takes up to 60m and defaults to 10m"));
    assert!(HELP.contains("--require-no-findings true"));
}

#[test]
fn approval_requires_an_explicit_exact_revision() {
    let revision = "0123456789abcdef0123456789abcdef";
    let options = parse_cmd(&["approve", "--deletion-set", revision]).unwrap();
    assert_eq!(options.path, "/_internal/sync/approve");
    assert_eq!(options.body, Some(json!({"deletion_set":revision})));
    // --revision names the pages approval was read from, not the set it approves.
    let Err(CliError::Diagnostics(refused)) = parse_cmd(&["approve", "--revision", revision])
    else {
        panic!("approval with --revision must be refused with a diagnostic");
    };
    assert!(
        refused.errors[0].fix.contains("--deletion-set"),
        "{refused:?}"
    );
    for args in [
        vec!["approve"],
        vec!["approve", "--deletion-set", "all"],
        vec!["rescan", "--revision", revision],
        vec!["status", "--cursor", "x"],
        vec!["stop"],
    ] {
        assert!(parse_cmd(&args).is_err(), "{args:?}");
    }
    assert_eq!(parse_cmd(&["rescan"]).unwrap().body, Some(json!({})));
}

#[test]
fn pages_preserve_opaque_cursors_without_query_injection() {
    let options = parse_cmd(&[
        "deletions",
        "--revision",
        "0123456789abcdef0123456789abcdef",
        "--cursor",
        "x&revision=y+/",
        "--limit",
        "2",
    ])
    .unwrap();
    assert!(
        options
            .path
            .ends_with("&cursor=x%26revision%3Dy%2B%2F&limit=2")
    );
    assert!(options.body.is_none());
    assert!(parse_cmd(&["deletions"]).is_err());
    assert!(
        parse(&[
            "status",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--endpoint",
            "http://127.0.0.1:4444"
        ])
        .is_err()
    );
}

#[test]
fn findings_require_their_session_bound_revision() {
    let revision = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let page = parse_cmd(&["findings", "--revision", revision]).unwrap();
    assert_eq!(
        page.path,
        format!("/_internal/sync?view=findings&revision={revision}")
    );
    assert!(parse_cmd(&["findings", "--revision", &revision[..32]]).is_err());
    assert!(parse_cmd(&["approve", "--revision", revision]).is_err());
}

#[test]
fn conflicts_keep_their_own_endpoint_revision_and_opaque_cursor() {
    assert_eq!(
        parse_cmd(&["conflicts"]).unwrap().path,
        "/_internal/sync/conflicts"
    );
    let revision = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        parse_cmd(&[
            "conflicts",
            "--revision",
            revision,
            "--cursor",
            "a&b",
            "--limit",
            "2"
        ])
        .unwrap()
        .path,
        format!("/_internal/sync/conflicts?revision={revision}&cursor=a%26b&limit=2")
    );
    assert!(parse_cmd(&["conflicts", "--limit", "257"]).is_err());
}

#[test]
fn canonical_control_flags_map_to_the_exact_wire_contract() {
    let revision = "0123456789abcdef0123456789abcdef";
    let options = parse_cmd(&["approve", "--deletion-set", revision]).unwrap();
    assert_eq!(options.body, Some(json!({"deletion_set":revision})));
    assert!(
        parse_cmd(&[
            "approve",
            "--deletion-set",
            revision,
            "--revision",
            revision
        ])
        .is_err()
    );
    let options = parse_cmd(&["status", "--view", "deletions", "--revision", revision]).unwrap();
    assert_eq!(
        options.path,
        format!("/_internal/sync?view=deletions&revision={revision}")
    );
    let options = parse_cmd(&["wait", "--timeout", "10s"]).unwrap();
    assert_eq!(options.path, "/_internal/sync/wait");
    assert_eq!(options.timeout, Duration::from_secs(10));
    assert_eq!(options.wait, Some(Unmapped::Report));
    assert_eq!(Unmapped::Report.body(10_000), json!({"timeout_ms":10000}));
    for value in ["0ms", "61m", "1.5s", "18446744073709551615s"] {
        assert!(parse_cmd(&["wait", "--timeout", value]).is_err(), "{value}");
    }
    assert!(parse_cmd(&["wait", "--timeout", "1s", "--timeout-ms", "1000"]).is_err());
    let strict = parse_cmd(&["wait", "--require-no-findings", "true"]).unwrap();
    assert_eq!(strict.wait, Some(Unmapped::Refuse));
    assert_eq!(
        Unmapped::Refuse.body(25),
        json!({"timeout_ms":25,"require_no_findings":true})
    );
    assert_eq!(
        parse_cmd(&["wait", "--require-no-findings", "false"])
            .unwrap()
            .wait,
        Some(Unmapped::Report)
    );
    assert_eq!(
        refusal(&[
            "wait",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--require-no-findings",
            "yes"
        ]),
        "--require-no-findings must be true or false"
    );
    // Only a wait decides whether findings stop it.
    assert_eq!(
        refusal(&[
            "status",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--require-no-findings",
            "true"
        ]),
        "This sync operation does not accept the remaining options; see sync --help"
    );
}

#[test]
fn resolution_requires_exclusive_targets_and_matching_confirmation() {
    let id = "0123456789abcdef0123456789abcdef";
    let base = [
        "resolve",
        "--revision",
        id,
        "--action",
        "discard",
        "--operation-id",
        "operation-one",
    ];
    let mut args = base.to_vec();
    args.extend(["--conflict", id, "--expected-generation", "0"]);
    let options = parse_cmd(&args).unwrap();
    assert_eq!(options.path, "/_internal/sync/resolve");
    assert_eq!(
        options.body,
        Some(
            json!({"conflict_id":id,"revision":id,"action":"discard","expected_generation":0,"operation_id":"operation-one"})
        )
    );
    args.extend(["--recovery", id]);
    assert!(parse_cmd(&args).is_err());
    for confirmation in [
        "--confirm-backup-verified",
        "--confirm-discard-without-backup",
    ] {
        let mut args = base.to_vec();
        args.extend(["--recovery", id, "--target-identity", id]);
        assert!(parse_cmd(&args).is_err());
        args.extend([confirmation, id]);
        let body = parse_cmd(&args).unwrap().body.unwrap();
        assert_eq!(body["recovery_id"], id);
        assert_eq!(
            body["confirmation"],
            if confirmation == "--confirm-backup-verified" {
                "backup_verified"
            } else {
                "discard_without_backup"
            }
        );
        assert!(body.get("conflict_id").is_none());
        assert!(body.get("expected_generation").is_none());
        *args.last_mut().unwrap() = "ffffffffffffffffffffffffffffffff";
        assert!(parse_cmd(&args).is_err());
    }
    assert!(parse_cmd(&base).is_err());
}

/// A wait may last longer than the one minute a server observes per request, up to an hour.
#[test]
fn a_wait_takes_up_to_an_hour_while_other_controls_take_a_minute() {
    let options = parse_cmd(&["wait", "--timeout-ms", "25"]).unwrap();
    assert_eq!(options.timeout, Duration::from_millis(25));
    assert!(options.body.is_none());
    for (args, timeout) in [
        (
            vec!["wait", "--timeout-ms", "60001"],
            Duration::from_millis(60_001),
        ),
        (vec!["wait", "--timeout", "61s"], Duration::from_secs(61)),
        (vec!["wait", "--timeout", "60m"], Duration::from_secs(3600)),
        (vec!["status", "--timeout", "1m"], Duration::from_secs(60)),
    ] {
        assert_eq!(parse_cmd(&args).unwrap().timeout, timeout, "{args:?}");
    }
    let endpoint = "http://127.0.0.1:4443";
    for (args, fix) in [
        (
            vec!["wait", "--endpoint", endpoint, "--timeout-ms", "0"],
            "--timeout-ms must be 1 through 3600000",
        ),
        (
            vec!["wait", "--endpoint", endpoint, "--timeout-ms", "3600001"],
            "--timeout-ms must be 1 through 3600000",
        ),
        (
            vec!["wait", "--endpoint", endpoint, "--timeout", "3600001ms"],
            "--timeout must be 1ms through 60m",
        ),
        (
            vec!["rescan", "--endpoint", endpoint, "--timeout-ms", "60001"],
            "--timeout-ms must be 1 through 60000",
        ),
    ] {
        assert_eq!(refusal(&args), fix, "{args:?}");
    }
}

#[test]
fn local_endpoints_reject_credential_redirect_and_metadata_targets() {
    for value in [
        "http://localhost",
        "http://localhost:0",
        "http://user@localhost:80",
        "http://localhost:80/path",
        "http://localhost:80/?x",
        "http://localhost:80/#x",
        "ftp://localhost:80",
    ] {
        assert!(endpoint(value, None).is_err(), "{value}");
    }
    // Which resolved addresses count as local is tested with the shared check in `gcs_mimic::net`.
    assert!(parse_cmd(&["status", "--ca-cert", "unused"]).is_err());
    assert!(parse_cmd(&["status", "--timeout-ms", "60001"]).is_err());
}

fn mock_response(response: Vec<u8>) -> (Result<(StatusCode, Value), CliError>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let peer = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 1024];
        while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            assert_ne!(read, 0);
            bytes.extend_from_slice(&buffer[..read]);
        }
        let _ = stream.write_all(&response);
        String::from_utf8(bytes).unwrap()
    });
    let options = parse(&["status", "--endpoint", &endpoint]).unwrap();
    let result = options
        .execute(options.body.as_ref(), options.timeout)
        .map(|reply| (reply.status, reply.body));
    (result, peer.join().unwrap())
}

/// The status and reason a test server answers one wait request with, at once.
struct WaitAnswer {
    status: &'static str,
    reason: &'static str,
}

impl WaitAnswer {
    /// The answer a server gives when the deadline of the request passed.
    fn timed_out(reason: &'static str) -> Self {
        Self {
            status: "408 Request Timeout",
            reason,
        }
    }

    fn new(status: &'static str, reason: &'static str) -> Self {
        Self { status, reason }
    }
}

/// A server for one test that answers the n-th wait request with `answer(n)`. A connection that
/// sends no request ends it, and `requests` returns the `timeout_ms` of every request it got.
fn wait_server(
    answer: impl Fn(usize) -> WaitAnswer + Send + 'static,
) -> (String, thread::JoinHandle<Vec<u64>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let peer = thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 1024];
            let body = loop {
                let read = stream.read(&mut buffer).unwrap();
                if read == 0 && bytes.is_empty() {
                    return requests;
                }
                assert_ne!(read, 0);
                bytes.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&bytes).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if body.len() >= length {
                        break serde_json::from_str::<Value>(body).unwrap();
                    }
                }
            };
            let timeout_ms = body["timeout_ms"].as_u64().unwrap();
            let answer = answer(requests.len());
            requests.push(timeout_ms);
            let json =
                json!({"completed":answer.reason=="Ready","reason":answer.reason}).to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                    answer.status,
                    json.len()
                )
                .as_bytes(),
            );
        }
    });
    (endpoint, peer)
}

/// Ends a server from `wait_server` and returns the `timeout_ms` of every request it answered.
fn requests(endpoint: &str, server: thread::JoinHandle<Vec<u64>>) -> Vec<u64> {
    drop(std::net::TcpStream::connect(endpoint.strip_prefix("http://").unwrap()).unwrap());
    server.join().unwrap()
}

/// Runs `sync wait --timeout 150s` against `endpoint`, reading the time the wait has spent, in
/// seconds, from `elapsed` in turn instead of the clock.
fn wait_for_150s(endpoint: &str, elapsed: &[u64]) -> Reply {
    let options = parse(&["wait", "--endpoint", endpoint, "--timeout", "150s"]).unwrap();
    let mut elapsed = elapsed.iter().map(|seconds| Duration::from_secs(*seconds));
    options
        .wait_until_settled(options.wait.unwrap(), || {
            elapsed
                .next()
                .expect("the wait read the clock more often than expected")
        })
        .unwrap()
}

/// A wait outlasts the minute one request may take: the client asks again with the time left,
/// a minute at most, while the tree is still changing, until the server is ready.
#[test]
fn a_wait_asks_again_past_one_requests_minute_until_it_is_ready() {
    let (endpoint, server) = wait_server(|index| match index {
        0 => WaitAnswer::timed_out("Timeout"),
        1 => WaitAnswer::new("409 Conflict", "SourceChanging"),
        _ => WaitAnswer::new("200 OK", "Ready"),
    });
    let reply = wait_for_150s(&endpoint, &[0, 60, 60, 120, 120]);
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["reason"], "Ready");
    assert_eq!(requests(&endpoint, server), [60_000, 60_000, 30_000]);
}

/// Waiting again stops at the wait's own deadline, past several requests' minutes; the last
/// answer is the result.
#[test]
fn a_wait_keeps_asking_until_its_own_timeout() {
    let (endpoint, server) = wait_server(|_| WaitAnswer::timed_out("Timeout"));
    let reply = wait_for_150s(&endpoint, &[0, 60, 60, 120, 120, 150]);
    assert_eq!(reply.status, StatusCode::REQUEST_TIMEOUT);
    assert_eq!(reply.body["reason"], "Timeout");
    assert_eq!(requests(&endpoint, server), [60_000, 60_000, 30_000]);
}

/// A reason that only someone acting can change ends the wait with the first answer.
#[test]
fn a_wait_stops_at_once_for_what_waiting_cannot_change() {
    for reason in [
        "Paused",
        "FindingsPending",
        "DeletionApproval",
        "SourceUnrepresentable",
        "Stopped",
    ] {
        let (endpoint, server) = wait_server(move |_| WaitAnswer::new("409 Conflict", reason));
        let reply = wait_for_150s(&endpoint, &[0]);
        assert_eq!(reply.body["reason"], reason);
        assert_eq!(requests(&endpoint, server), [60_000], "{reason}");
    }
}

/// A wait that succeeds names the command that lists the objects the directory cannot hold, in
/// one line, and says nothing when none remain. One that fails names its reason and what to run
/// next, with the options that reached the server.
#[test]
fn wait_advice_names_the_command_to_run_next() {
    let connection = "--endpoint https://127.0.0.1:4443 --ca-cert ca.pem --token-file token";
    let revision = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let ready = json!({"reason":"Ready","findings":{"unmapped":2,"revision":revision}});
    let advice = WaitOutcome::read(&ready).advice(connection);
    assert!(!advice.is_err());
    let [hint] = advice.warnings.as_slice() else {
        panic!("one hint expected: {advice:?}");
    };
    assert!(
        hint.fix
            .starts_with("2 objects in the API cannot be written"),
        "{}",
        hint.fix
    );
    assert!(
        hint.fix.ends_with(&format!(
            "gcs-mimic sync status --view findings --revision {revision} {connection}"
        )),
        "{}",
        hint.fix
    );
    let clear = json!({"reason":"Ready","findings":{"unmapped":0,"revision":revision}});
    let advice = WaitOutcome::read(&clear).advice(connection);
    assert!(
        advice.warnings.is_empty() && advice.errors.is_empty(),
        "{advice:?}"
    );
    let paused = json!({"reason":"Paused","paused_reason":"IoFailure","findings":null,
        "remedy":"Restore access, then run gcs-mimic sync rescan --endpoint URL"});
    let advice = WaitOutcome::read(&paused).advice(connection);
    let [error] = advice.errors.as_slice() else {
        panic!("one error expected: {advice:?}");
    };
    assert_eq!(
        error.fix,
        format!(
            "sync wait ended with Paused (paused_reason IoFailure): Restore access, then run gcs-mimic sync rescan {connection}"
        )
    );
    assert!(advice.warnings.is_empty());
    let inputs = crate::test_support::TestDir::new();
    let token = inputs.0.join("token");
    std::fs::write(&token, b"secret\n").unwrap();
    let token = token.to_str().unwrap();
    let options = parse(&[
        "wait",
        "--endpoint",
        "https://127.0.0.1:4443",
        "--ca-cert",
        "ca.pem",
        "--token-file",
        token,
    ])
    .unwrap();
    assert_eq!(
        options.connection(),
        format!("--endpoint https://127.0.0.1:4443 --ca-cert ca.pem --token-file {token}")
    );
    let options = parse(&["wait", "--endpoint", "http://127.0.0.1:4443"]).unwrap();
    assert_eq!(options.connection(), "--endpoint http://127.0.0.1:4443");
}

/// Every command that advice can name parses as this command line once the options that
/// reached a server replace its endpoint. Every piece of advice names commands only by rendering
/// them: what the server answers and logs, the healthcheck diagnostic, and this command's own
/// advice after a wait, which fills in the endpoint.
#[test]
fn every_advised_command_parses_as_this_command_line() {
    let inputs = crate::test_support::TestDir::new();
    let token = inputs.0.join("token");
    std::fs::write(&token, b"secret\n").unwrap();
    let connections = [
        "--endpoint http://127.0.0.1:4443".to_owned(),
        "--endpoint https://[::1]:4443 --ca-cert tls/ca.pem".to_owned(),
        format!(
            "--endpoint https://127.0.0.1:4443 --token-file {}",
            token.display()
        ),
    ];
    let commands = advice::commands();
    for command in &commands {
        for connection in &connections {
            let line = advice::with_connection(command, connection);
            let args = line.strip_prefix("gcs-mimic sync ").unwrap();
            let args: Vec<String> = args.split(' ').map(str::to_owned).collect();
            if let Err(error) = Options::parse(&args) {
                panic!("{line}: {error:?}");
            }
        }
    }

    // Each piece of advice, with the connection that replaced its endpoint.
    let catalog = advice::catalog();
    let mut advised: Vec<(String, String)> = catalog
        .iter()
        .map(|advice| (advice.clone(), advice::ENDPOINT.to_owned()))
        .collect();
    let starting = br#"{"status":"ok","unix_time":1,"sync":{"state":"starting"}}"#;
    for (url, ca_cert) in [
        ("http://127.0.0.1:4443/_internal/ready", None),
        (
            "https://[::1]:4443/_internal/ready",
            Some(Path::new("tls/ca.pem")),
        ),
    ] {
        let url = url.parse().unwrap();
        let Err(diagnostics) = crate::synchronization_started(&url, ca_cert, starting) else {
            panic!("a starting mirror passed the healthcheck");
        };
        let connection = crate::probe_connection(&url, ca_cert);
        advised.extend(
            diagnostics
                .errors
                .into_iter()
                .map(|error| (error.fix, connection.clone())),
        );
    }
    let connection = &connections[1];
    for remedy in &catalog {
        let answer = json!({"reason":"FindingsPending","remedy":remedy,
            "findings":{"unmapped":1,"revision":advice::SAMPLE_FINDINGS_REVISION}});
        let diagnostics = WaitOutcome::read(&answer).advice(connection);
        let given = diagnostics.warnings.into_iter().chain(diagnostics.errors);
        advised.extend(given.map(|diagnostic| (diagnostic.fix, connection.clone())));
    }

    for (advice, connection) in &advised {
        let rendered: Vec<String> = commands
            .iter()
            .map(|command| advice::with_connection(command, connection))
            .collect();
        let named = advice.match_indices("gcs-mimic sync ").map(|(at, _)| {
            let text = &advice[at..];
            // A rendered command followed by another argument would be one it does not parse.
            rendered.iter().any(|command| {
                text.strip_prefix(command.as_str()).is_some_and(|rest| {
                    !rest.starts_with(" -") && !rest.starts_with(char::is_alphanumeric)
                })
            })
        });
        let named: Vec<bool> = named.collect();
        assert!(
            !named.is_empty() && named.iter().all(|rendered| *rendered),
            "{advice}"
        );
    }
}

#[test]
fn wire_client_decodes_chunked_json_and_preserves_failure_status() {
    let (result, request) = mock_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{}\r\n0\r\n\r\n".to_vec());
    assert_eq!(result.unwrap(), (StatusCode::OK, json!({})));
    assert!(request.starts_with("GET /_internal/sync HTTP/1.1\r\n"));
    assert!(
        request.contains(&format!("x-gcs-mimic-version: {}\r\n", gcs_mimic::VERSION)),
        "{request}"
    );
    let (result, _) = mock_response(b"HTTP/1.1 409 Conflict\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"error\":\"stale\"}".to_vec());
    assert_eq!(
        result.unwrap(),
        (StatusCode::CONFLICT, json!({"error":"stale"}))
    );
    let (result, _) = mock_response(b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec());
    assert_eq!(result.unwrap().0, StatusCode::FOUND);
}

#[test]
fn non_json_responses_keep_the_status_without_echoing_the_body() {
    for (status, body) in [
        (
            StatusCode::BAD_GATEWAY,
            b"private proxy diagnostic".as_slice(),
        ),
        (StatusCode::NO_CONTENT, b"".as_slice()),
    ] {
        let error = response_json(status, body).unwrap_err();
        assert!(error.to_string().contains(&status.to_string()));
        assert!(!error.to_string().contains("private proxy diagnostic"));
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<serde_json::Error>()
        );
    }
}

#[test]
fn wire_client_rejects_oversized_and_truncated_responses() {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        RESPONSE_LIMIT + 1
    )
    .into_bytes();
    response.extend(vec![b' '; RESPONSE_LIMIT + 1]);
    assert!(mock_response(response).0.is_err());
    assert!(
        mock_response(
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{}".to_vec()
        )
        .0
        .is_err()
    );
}
