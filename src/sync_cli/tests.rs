use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn parse(args: &[&str]) -> Result<Options, CliError> {
    Options::parse(
        &args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        Vec::new(),
    )
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
        // An unknown operation is reported only after the shared options are read, and an
        // endpoint found in the environment.
        (
            vec!["stop", "--ca-cert", "ca"],
            "--ca-cert requires an HTTPS endpoint, and the environment gives http://127.0.0.1:4443; set GCS_MIMIC_SCHEME as the server has it or supply --endpoint http(s)://HOST:PORT",
        ),
        (
            vec!["stop"],
            "Use sync status, findings, deletions, conflicts, rescan, approve, resolve or wait with their documented options",
        ),
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
            vec!["deletions", "--endpoint", endpoint, "--cursor", "x"],
            "A --cursor continues the pages of one revision; supply the --revision of the page that returned it",
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
    // A wait asks again until its own timeout, which covers the changes of a test step on a
    // server the healthcheck already found healthy, after its first pass.
    assert_eq!(
        parse_cmd(&["wait"]).unwrap().timeout,
        Duration::from_secs(120)
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
    assert!(HELP.contains("Wait takes up to 60m and defaults to 2m"));
    assert!(HELP.contains("--require-no-findings, which takes no\nvalue"));
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
    assert!(options.latest.is_none());
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
    assert_eq!(options.wait, Some(UnmappedObjects::Report));
    assert_eq!(
        wait_body(UnmappedObjects::Report, 10_000),
        json!({"timeout_ms":10000})
    );
    for value in ["0ms", "61m", "1.5s", "18446744073709551615s"] {
        assert!(parse_cmd(&["wait", "--timeout", value]).is_err(), "{value}");
    }
    assert!(parse_cmd(&["wait", "--timeout", "1s", "--timeout-ms", "1000"]).is_err());
    // --require-no-findings is a switch, wherever it stands among the options.
    for args in [
        &["wait", "--require-no-findings"][..],
        &["wait", "--require-no-findings", "--timeout", "10s"],
        &["wait", "--timeout", "10s", "--require-no-findings"],
    ] {
        let strict = parse_cmd(args).unwrap();
        assert_eq!(strict.wait, Some(UnmappedObjects::Refuse), "{args:?}");
    }
    assert_eq!(
        wait_body(UnmappedObjects::Refuse, 25),
        json!({"timeout_ms":25,"require_no_findings":true})
    );
    // A value given to it says that it takes none, in either spelling.
    for (value, given) in [
        (&["--require-no-findings", "true"][..], "true"),
        (
            &["--require-no-findings", "false", "--timeout", "1s"],
            "false",
        ),
        (&["--require-no-findings=true"], "=true"),
    ] {
        let mut args = vec!["wait", "--endpoint", "http://127.0.0.1:4443"];
        args.extend_from_slice(value);
        assert_eq!(
            refusal(&args),
            format!(
                "--require-no-findings is a switch and takes no value; give it alone, without {given}"
            )
        );
    }
    assert_eq!(
        refusal(&[
            "wait",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--require-no-findings",
            "--require-no-findings"
        ]),
        "Use unique sync option/value pairs; see sync --help"
    );
    // Only a wait decides whether findings stop it.
    assert_eq!(
        refusal(&[
            "status",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--require-no-findings"
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
        assert!(
            endpoint(value, None, EndpointSource::Given).is_err(),
            "{value}"
        );
    }
    // Which resolved addresses count as local is tested with the shared check in `gcs_mimic::net`.
    assert!(parse_cmd(&["status", "--ca-cert", "unused"]).is_err());
    assert!(parse_cmd(&["status", "--timeout-ms", "60001"]).is_err());
}

/// Without --endpoint, the command controls the server that the environment configures, as the
/// healthcheck finds it, and the commands it advises leave the endpoint out. An explicit endpoint
/// wins. A config file, which the command does not open, a configuration with errors and an
/// origin the command cannot reach each say what to supply instead.
#[test]
fn without_an_endpoint_the_command_controls_the_server_the_environment_configures() {
    let parse_in = |args: &[&str], environment: &[(&str, &str)]| {
        Options::parse(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            environment
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value)))
                .collect(),
        )
    };
    let options = parse_in(&["status"], &[]).unwrap();
    assert_eq!(options.endpoint.authority.as_str(), "127.0.0.1:4443");
    assert!(matches!(options.endpoint.transport, Transport::Http));
    assert_eq!(options.connection(), "");
    let paused = json!({"reason":"Paused","paused_reason":"IoFailure",
        "remedy":"Restore access; then resume synchronization: gcs-mimic sync rescan --endpoint URL"});
    let advice = WaitOutcome::read(&paused).advice(&options.connection());
    assert!(
        advice.errors[0]
            .fix
            .ends_with("resume synchronization: gcs-mimic sync rescan"),
        "{advice:?}"
    );

    let https = [
        ("GCS_MIMIC_LISTEN", "0.0.0.0:9000"),
        ("GCS_MIMIC_SCHEME", "https"),
    ];
    let options = parse_in(&["wait", "--ca-cert", "ca.pem"], &https).unwrap();
    assert_eq!(options.endpoint.authority.as_str(), "127.0.0.1:9000");
    assert!(matches!(
        options.endpoint.transport,
        Transport::Https { ca_cert: Some(_) }
    ));
    assert_eq!(options.connection(), "--ca-cert ca.pem");
    let config = [("GCS_MIMIC_CONFIG", "server.json")];
    let options = parse_in(&["status", "--endpoint", "http://127.0.0.1:4000"], &config).unwrap();
    assert_eq!(options.connection(), "--endpoint http://127.0.0.1:4000");

    let supply = "supply --endpoint http(s)://HOST:PORT";
    for (environment, fix) in [
        (
            config.as_slice(),
            format!(
                "GCS_MIMIC_CONFIG selects a config file, which sync does not open to find the server; {supply}"
            ),
        ),
        (
            &[("GCS_MIMIC_LISTEN", "127.0.0.1:0")],
            format!(
                "The environment gives the endpoint http://127.0.0.1:0, which sync cannot reach; set GCS_MIMIC_LISTEN as the server has it or {supply}"
            ),
        ),
    ] {
        let Err(CliError::Diagnostics(refused)) = parse_in(&["status"], environment) else {
            panic!("{environment:?}: refused without a diagnostic");
        };
        assert_eq!(refused.errors.len(), 1, "{refused:?}");
        assert_eq!(refused.errors[0].fix, fix);
    }
    let Err(CliError::Diagnostics(refused)) =
        parse_in(&["status"], &[("GCS_MIMIC_LISTEN", "garbage")])
    else {
        panic!("a listen address that does not parse was accepted");
    };
    let [setting, endpoint] = refused.errors.as_slice() else {
        panic!("the setting and the endpoint expected: {refused:?}");
    };
    assert_eq!(setting.key, "GCS_MIMIC_LISTEN");
    assert!(
        endpoint
            .fix
            .starts_with("The server configuration in the environment has errors"),
        "{endpoint:?}"
    );
    assert!(endpoint.fix.ends_with(supply), "{endpoint:?}");
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
        .execute(&options.path, options.body.as_ref(), options.timeout)
        .map(|reply| (reply.status, reply.body));
    (result, peer.join().unwrap())
}

/// A server for one test that answers its requests in turn with `answers`, each a status line
/// and a JSON body from this release, and returns the target of each request.
fn page_server(answers: Vec<(&'static str, Value)>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let peer = thread::spawn(move || {
        answers
            .into_iter()
            .map(|(status, body)| {
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
                let json = body.to_string();
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{}: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        handshake::VERSION_HEADER,
                        gcs_mimic::VERSION,
                        json.len()
                    )
                    .as_bytes(),
                );
                let head = String::from_utf8(bytes).unwrap();
                head.split(' ').nth(1).unwrap().to_owned()
            })
            .collect()
    });
    (endpoint, peer)
}

/// A view without --revision reads the status first and lists the page at the revision it
/// reports, keeping the page size; when that revision changed before the page was read, it reads
/// the status once more. A deletion view with no set awaiting approval says so, and a cursor,
/// which belongs to one revision, needs that revision.
#[test]
fn a_view_without_a_revision_lists_the_page_at_the_revision_status_reports() {
    let findings = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let newer = "0123456789abcdef0123456789abcdef0123456789abcde0";
    let (endpoint, server) = page_server(vec![
        ("200 OK", json!({"findings_revision":findings})),
        ("409 Conflict", json!({"error":{"code":409}})),
        ("200 OK", json!({"findings_revision":newer})),
        ("200 OK", json!({"view":"findings","revision":newer})),
    ]);
    let options = parse(&[
        "status",
        "--endpoint",
        &endpoint,
        "--view",
        "findings",
        "--limit",
        "2",
    ])
    .unwrap();
    assert_eq!(options.path, "/_internal/sync");
    let page = options.latest.as_ref().unwrap();
    let reply = options.latest_page(page).unwrap();
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["revision"], newer);
    assert_eq!(
        server.join().unwrap(),
        [
            "/_internal/sync".to_owned(),
            format!("/_internal/sync?view=findings&revision={findings}&limit=2"),
            "/_internal/sync".to_owned(),
            format!("/_internal/sync?view=findings&revision={newer}&limit=2"),
        ]
    );

    let (endpoint, server) =
        page_server(vec![("200 OK", json!({"pending_deletion_revision":null}))]);
    let options = parse(&["deletions", "--endpoint", &endpoint]).unwrap();
    let Err(CliError::Diagnostics(refused)) = options.latest_page(options.latest.as_ref().unwrap())
    else {
        panic!("a deletion view without a pending set must be refused with a diagnostic");
    };
    assert_eq!(
        refused.errors[0].fix,
        "No deletion set awaits approval: status reports no pending_deletion_revision"
    );
    assert_eq!(server.join().unwrap(), ["/_internal/sync"]);
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

/// A wait's exit status tells how it ended: 0 when ready, 2 when someone must act, 3 when its
/// timeout passed while work it asks again about was in progress, and 1 for a stopped server
/// or an answer without a reason, like any other failed command.
#[test]
fn a_wait_exits_with_the_status_of_how_it_ended() {
    use WaitExit::{ActionNeeded, Failed, Ready, TimedOut};
    for (name, exit) in [
        ("Ready", Ready),
        ("Timeout", TimedOut),
        ("Stopped", Failed),
        ("Paused", ActionNeeded),
        ("JournalChanged", TimedOut),
        ("SourceChanging", TimedOut),
        ("SourceUnrepresentable", ActionNeeded),
        ("DeletionApproval", ActionNeeded),
        ("FindingsPending", ActionNeeded),
    ] {
        let reason: WaitReason = name.parse().unwrap();
        assert_eq!(WaitExit::of(Some(reason)), exit, "{name}");
        assert_eq!(reason.is_in_progress(), exit == TimedOut, "{name}");
    }
    assert_eq!(WaitExit::of(None), Failed);
    assert_eq!(
        [Ready, Failed, ActionNeeded, TimedOut].map(WaitExit::code),
        [0, 1, 2, 3]
    );
    let ended = |answer: Value, status| match WaitOutcome::read(&answer)
        .conclude(status, "--endpoint http://127.0.0.1:4443")
    {
        Ok(()) => (Ready, Vec::new()),
        Err(CliError::Wait(diagnostics, exit)) => (exit, diagnostics.errors),
        Err(CliError::Diagnostics(diagnostics)) => (Failed, diagnostics.errors),
        Err(error) => panic!("{error:?}"),
    };
    assert_eq!(ended(json!({"reason":"Ready"}), StatusCode::OK).0, Ready);
    // A wait that the objects it refuses failed exits as one that needs action, with its remedy.
    let (exit, errors) = ended(
        json!({"reason":"FindingsPending","findings":{"unmapped":1,"revision":"r"},
            "remedy":"Rename it: gcs-mimic sync wait --endpoint URL"}),
        StatusCode::CONFLICT,
    );
    assert_eq!(exit, ActionNeeded);
    assert_eq!(
        errors[0].fix,
        "sync wait ended with FindingsPending: Rename it: gcs-mimic sync wait --endpoint http://127.0.0.1:4443"
    );
    assert_eq!(
        ended(json!({"reason":"Timeout"}), StatusCode::REQUEST_TIMEOUT).0,
        TimedOut
    );
    let (exit, errors) = ended(json!({"reason":"Stopped"}), StatusCode::CONFLICT);
    assert_eq!(
        (exit, errors[0].fix.as_str()),
        (Failed, "sync wait ended with Stopped")
    );
    let (exit, errors) = ended(
        json!({"error":{"code":503}}),
        StatusCode::SERVICE_UNAVAILABLE,
    );
    assert_eq!(exit, Failed);
    assert!(errors[0].fix.contains("HTTP 503"), "{errors:?}");
    assert_eq!(
        crate::CliError::Wait(Diagnostics::default(), TimedOut).exit_code(),
        std::process::ExitCode::from(3)
    );
    assert_eq!(
        crate::CliError::Diagnostics(Diagnostics::default()).exit_code(),
        std::process::ExitCode::FAILURE
    );
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
    // A wait that the objects failed names them in its one error, without the success's hint.
    let refused = json!({"reason":"FindingsPending","findings":{"unmapped":2,"revision":revision},
        "remedy":"2 objects remain; the findings view lists them: gcs-mimic sync status --endpoint URL"});
    let advice = WaitOutcome::read(&refused).advice(connection);
    assert!(advice.warnings.is_empty(), "{advice:?}");
    assert_eq!(advice.errors.len(), 1, "{advice:?}");
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
        // A command that found the server in the environment advises commands that find it there.
        String::new(),
    ];
    let commands = advice::commands();
    for command in &commands {
        for connection in &connections {
            let line = advice::with_connection(command, connection);
            let args = line.strip_prefix("gcs-mimic sync ").unwrap();
            let args: Vec<String> = args.split(' ').map(str::to_owned).collect();
            if let Err(error) = Options::parse(&args, Vec::new()) {
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
    for connection in [&connections[1], &connections[3]] {
        let ready = json!({"reason":"Ready",
            "findings":{"unmapped":1,"revision":advice::SAMPLE_FINDINGS_REVISION}});
        let answers = catalog.iter().map(|remedy| {
            json!({"reason":"FindingsPending","remedy":remedy,
                "findings":{"unmapped":1,"revision":advice::SAMPLE_FINDINGS_REVISION}})
        });
        for answer in answers.chain([ready]) {
            let diagnostics = WaitOutcome::read(&answer).advice(connection);
            let given = diagnostics.warnings.into_iter().chain(diagnostics.errors);
            advised.extend(given.map(|diagnostic| (diagnostic.fix, connection.clone())));
        }
    }

    for (advice, connection) in &advised {
        let rendered: Vec<String> = commands
            .iter()
            .map(|command| advice::with_connection(command, connection))
            .collect();
        let named = named_commands(advice);
        assert!(!named.is_empty(), "{advice}");
        for at in named {
            // Only `;` or the end of the text may follow a command: an argument, a word or a
            // comma after it reads as part of the line a reader copies.
            let whole = advice[at..].strip_prefix("gcs-mimic ").is_some_and(|_| {
                rendered.iter().any(|command| {
                    advice[at..]
                        .strip_prefix(command.as_str())
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with(';'))
                })
            });
            assert!(whole, "{advice}");
        }
    }
}

/// Where `advice` names a command: each `gcs-mimic sync`, and each subcommand named after
/// `sync` without it, such as `sync approve,`, which a reader would run without the
/// connection. A subcommand that a sentence goes on from, as in `sync wait ended`, is prose.
fn named_commands(advice: &str) -> Vec<usize> {
    const SUBCOMMANDS: [&str; 6] = [
        "status",
        "rescan",
        "wait",
        "approve",
        "resolve",
        "conflicts",
    ];
    let mut named: Vec<usize> = advice
        .match_indices("gcs-mimic sync ")
        .map(|(at, _)| at)
        .collect();
    for (at, _) in advice.match_indices("sync ") {
        let after = &advice[at + "sync ".len()..];
        let bare = !advice[..at].ends_with("gcs-mimic ");
        let command = SUBCOMMANDS.iter().any(|sub| {
            after.strip_prefix(sub).is_some_and(|rest| {
                rest.is_empty()
                    || rest.starts_with(" -")
                    || rest.starts_with(|c: char| c.is_ascii_punctuation())
            })
        });
        if bare && command {
            named.push(at);
        }
    }
    named
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
