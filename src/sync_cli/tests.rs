use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn parse(args: &[&str]) -> io::Result<Options> {
    Options::parse(&args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
}

#[test]
fn approval_requires_an_explicit_exact_revision() {
    let revision = "0123456789abcdef0123456789abcdef";
    let options = parse(&[
        "approve",
        "--endpoint",
        "http://127.0.0.1:4443",
        "--revision",
        revision,
    ])
    .unwrap();
    assert_eq!(options.path, "/_internal/sync/approve");
    assert_eq!(options.body, Some(json!({"deletion_set":revision})));
    for args in [
        vec!["approve", "--endpoint", "http://127.0.0.1:4443"],
        vec![
            "approve",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--revision",
            "all",
        ],
        vec![
            "rescan",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--revision",
            revision,
        ],
        vec![
            "status",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--cursor",
            "x",
        ],
        vec!["stop", "--endpoint", "http://127.0.0.1:4443"],
    ] {
        assert!(parse(&args).is_err(), "{args:?}");
    }
    assert_eq!(
        parse(&["rescan", "--endpoint", "http://127.0.0.1:4443"])
            .unwrap()
            .body,
        Some(json!({}))
    );
}

#[test]
fn pages_preserve_opaque_cursors_without_query_injection() {
    let options = parse(&[
        "deletions",
        "--endpoint",
        "http://127.0.0.1:4443",
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
    assert!(parse(&["deletions", "--endpoint", "http://127.0.0.1:4443"]).is_err());
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
    let page = parse(&[
        "findings",
        "--endpoint",
        "http://127.0.0.1:4443",
        "--revision",
        revision,
    ])
    .unwrap();
    assert_eq!(
        page.path,
        format!("/_internal/sync?view=findings&revision={revision}")
    );
    assert!(
        parse(&[
            "findings",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--revision",
            &revision[..32]
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "approve",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--revision",
            revision
        ])
        .is_err()
    );
}

#[test]
fn conflicts_keep_their_own_endpoint_revision_and_opaque_cursor() {
    let origin = "http://127.0.0.1:4443";
    assert_eq!(
        parse(&["conflicts", "--endpoint", origin]).unwrap().path,
        "/_internal/sync/conflicts"
    );
    let revision = "0123456789abcdef0123456789abcdef";
    assert_eq!(
        parse(&[
            "conflicts",
            "--endpoint",
            origin,
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
    assert!(parse(&["conflicts", "--endpoint", origin, "--limit", "257"]).is_err());
}

#[test]
fn canonical_control_flags_map_to_the_exact_wire_contract() {
    let origin = "http://127.0.0.1:4443";
    let revision = "0123456789abcdef0123456789abcdef";
    let options = parse(&["approve", "--endpoint", origin, "--deletion-set", revision]).unwrap();
    assert_eq!(options.body, Some(json!({"deletion_set":revision})));
    assert!(
        parse(&[
            "approve",
            "--endpoint",
            origin,
            "--deletion-set",
            revision,
            "--revision",
            revision
        ])
        .is_err()
    );
    let options = parse(&[
        "status",
        "--view",
        "deletions",
        "--endpoint",
        origin,
        "--revision",
        revision,
    ])
    .unwrap();
    assert_eq!(
        options.path,
        format!("/_internal/sync?view=deletions&revision={revision}")
    );
    let options = parse(&["wait", "--endpoint", origin, "--timeout", "10s"]).unwrap();
    assert_eq!(options.body, Some(json!({"timeout_ms":10000})));
    assert_eq!(options.timeout, Duration::from_secs(11));
    for value in ["0ms", "61s", "1.5s", "18446744073709551615s"] {
        assert!(parse(&["wait", "--endpoint", origin, "--timeout", value]).is_err());
    }
    assert!(
        parse(&[
            "wait",
            "--endpoint",
            origin,
            "--timeout",
            "1s",
            "--timeout-ms",
            "1000"
        ])
        .is_err()
    );
}

#[test]
fn resolution_requires_exclusive_targets_and_matching_confirmation() {
    let origin = "http://127.0.0.1:4443";
    let id = "0123456789abcdef0123456789abcdef";
    let base = [
        "resolve",
        "--endpoint",
        origin,
        "--revision",
        id,
        "--action",
        "discard",
        "--operation-id",
        "operation-one",
    ];
    let mut args = base.to_vec();
    args.extend(["--conflict", id, "--expected-generation", "0"]);
    let options = parse(&args).unwrap();
    assert_eq!(options.path, "/_internal/sync/resolve");
    assert_eq!(
        options.body,
        Some(
            json!({"conflict_id":id,"revision":id,"action":"discard","expected_generation":0,"operation_id":"operation-one"})
        )
    );
    args.extend(["--recovery", id]);
    assert!(parse(&args).is_err());
    for confirmation in [
        "--confirm-backup-verified",
        "--confirm-discard-without-backup",
    ] {
        let mut args = base.to_vec();
        args.extend(["--recovery", id, "--target-identity", id]);
        assert!(parse(&args).is_err());
        args.extend([confirmation, id]);
        let body = parse(&args).unwrap().body.unwrap();
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
        assert!(parse(&args).is_err());
    }
    assert!(parse(&base).is_err());
}

#[test]
fn waits_send_the_bounded_budget_and_allow_time_for_the_error_response() {
    let options = parse(&[
        "wait",
        "--endpoint",
        "http://127.0.0.1:4443",
        "--timeout-ms",
        "25",
    ])
    .unwrap();
    assert_eq!(options.body, Some(json!({"timeout_ms":25})));
    assert_eq!(options.path, "/_internal/sync/wait");
    assert_eq!(options.timeout, Duration::from_millis(1025));
    assert!(
        parse(&[
            "wait",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--timeout-ms",
            "0"
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "wait",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--timeout-ms",
            "60001"
        ])
        .is_err()
    );
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
        assert!(origin(value).is_err(), "{value}");
    }
    for value in [
        "127.0.0.1",
        "10.0.0.1",
        "192.168.1.1",
        "::1",
        "fc00::1",
        "::ffff:127.0.0.1",
    ] {
        assert!(local_address(value.parse().unwrap()));
    }
    for value in [
        "0.0.0.0",
        "169.254.169.254",
        "8.8.8.8",
        "::",
        "fe80::1",
        "ff02::1",
        "::ffff:169.254.169.254",
    ] {
        assert!(!local_address(value.parse().unwrap()));
    }
    assert!(
        parse(&[
            "status",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--ca-cert",
            "unused"
        ])
        .is_err()
    );
    assert!(
        parse(&[
            "status",
            "--endpoint",
            "http://127.0.0.1:4443",
            "--timeout-ms",
            "60001"
        ])
        .is_err()
    );
}

fn mock_response(response: Vec<u8>) -> (io::Result<(StatusCode, Value)>, String) {
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
    let result = options.execute();
    (result, peer.join().unwrap())
}

#[test]
fn wire_client_decodes_chunked_json_and_preserves_failure_status() {
    let (result, request) = mock_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{}\r\n0\r\n\r\n".to_vec());
    assert_eq!(result.unwrap(), (StatusCode::OK, json!({})));
    assert!(request.starts_with("GET /_internal/sync HTTP/1.1\r\n"));
    let (result, _) = mock_response(b"HTTP/1.1 409 Conflict\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"error\":\"stale\"}".to_vec());
    assert_eq!(
        result.unwrap(),
        (StatusCode::CONFLICT, json!({"error":"stale"}))
    );
    let (result, _) = mock_response(b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec());
    assert_eq!(result.unwrap().0, StatusCode::FOUND);
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
