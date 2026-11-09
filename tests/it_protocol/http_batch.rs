use crate::common;

use common::TestDir;
use common::ingress::Server;

const TYPE: &str = "Content-Type: multipart/mixed; boundary=\"batch-boundary\"\r\n";
const BUCKET: &str = "/storage/v1/b/batch-bucket";

fn part(id: Option<&str>, method: &str, path: &str, headers: &str, body: &str) -> String {
    let id = id
        .map(|id| format!("Content-ID: {id}\r\n"))
        .unwrap_or_default();
    format!(
        "--batch-boundary\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\n{id}\r\n{method} {path} HTTP/1.1\r\n{headers}\r\n{body}\r\n"
    )
}

fn envelope(parts: &[String]) -> Vec<u8> {
    format!("{}--batch-boundary--\r\n", parts.concat()).into_bytes()
}

/// Rewrites a CRLF envelope with LF line endings throughout, as some clients frame one.
fn lf(envelope: &[u8]) -> Vec<u8> {
    String::from_utf8(envelope.to_vec())
        .unwrap()
        .replace("\r\n", "\n")
        .into_bytes()
}

fn upload(server: &Server, name: &str) {
    server
        .request(
            "POST",
            &format!("/upload/storage/v1/b/batch-bucket/o?uploadType=media&name={name}"),
            "",
            b"content",
        )
        .status(200);
}

fn setup(server: &Server, auth: &str) {
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            &format!("Content-Type: application/json\r\n{auth}"),
            br#"{"name":"batch-bucket"}"#,
        )
        .status(200);
    for name in ["object", "a%2Fb"] {
        server
            .request(
                "POST",
                &format!("/upload/storage/v1/b/batch-bucket/o?uploadType=media&name={name}"),
                auth,
                b"content",
            )
            .status(200);
    }
}

#[test]
fn batch_dispatches_metadata_conditions_and_individual_errors_without_self_connections() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--max-connections", "1"]);
    setup(&server, "");
    let body = envelope(&[
        part(
            Some("<update>"),
            "PATCH",
            &format!("{BUCKET}/o/object?ifMetagenerationMatch=1"),
            "Content-Type: application/json\r\nContent-Length: 26\r\n",
            r#"{"metadata":{"key":"yes"}}"#,
        ),
        part(
            Some("<missing>"),
            "GET",
            &format!("{BUCKET}/o/missing"),
            "",
            "",
        ),
        part(None, "DELETE", &format!("{BUCKET}/o/a%2Fb"), "", ""),
        part(
            Some("<stale>"),
            "PATCH",
            &format!("{BUCKET}/o/object?ifMetagenerationMatch=0"),
            "Content-Type: application/json\r\n",
            "{}",
        ),
    ]);
    let reply = server.request("POST", "/batch/storage/v1", TYPE, &body);
    reply.status(200);
    assert!(
        reply
            .headers
            .contains("content-type: multipart/mixed; boundary=gcs_mimic_batch_response\r\n")
    );
    let statuses: Vec<_> = reply
        .body
        .lines()
        .filter(|line| line.starts_with("HTTP/1.1 "))
        .collect();
    assert_eq!(
        statuses,
        [
            "HTTP/1.1 200 OK",
            "HTTP/1.1 404 Not Found",
            "HTTP/1.1 204 No Content",
            "HTTP/1.1 412 Precondition Failed"
        ]
    );
    assert!(reply.body.contains("Content-ID: <response-update>\r\n"));
    assert!(reply.body.contains("Content-ID: <response-missing>\r\n"));
    assert!(reply.body.ends_with("--gcs_mimic_batch_response--\r\n"));
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/o/object"), "", b"")
            .json()["metadata"]["key"],
        "yes"
    );
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/o/object?alt=media"), "", b"")
            .body,
        "content"
    );
    server
        .request("GET", &format!("{BUCKET}/o/a%2Fb"), "", b"")
        .status(404);
}

#[test]
fn batch_inherits_query_and_headers_but_checks_each_inner_credential() {
    let dir = TestDir::new();
    let registry = common::process::write_identities(
        &dir.0,
        &serde_json::json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}},
            {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
        ]}),
    );
    let server = Server::start(
        &dir,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    setup(&server, "Authorization: Bearer admin\r\n");
    let headers = format!("{TYPE}Authorization: Bearer admin\r\n");
    let body = envelope(&[
        part(
            Some("get"),
            "GET",
            &format!("{BUCKET}/o/object?fields=name"),
            "",
            "",
        ),
        part(
            Some("denied"),
            "DELETE",
            &format!("{BUCKET}/o/object"),
            "Authorization: Bearer viewer\r\n",
            "",
        ),
        part(
            Some("invalid"),
            "GET",
            &format!("{BUCKET}/o/object"),
            "Authorization: Bearer invalid\r\n",
            "",
        ),
        part(
            Some("override"),
            "GET",
            &format!("{BUCKET}/o/object?fields=metageneration"),
            "",
            "",
        ),
        part(
            Some("query-credential"),
            "DELETE",
            &format!("{BUCKET}/o/object?access_token=admin"),
            "",
            "",
        ),
    ]);
    let reply = server.request("POST", "/batch/storage/v1?fields=name", &headers, &body);
    reply.status(200);
    assert!(reply.body.contains("HTTP/1.1 403 Forbidden"));
    assert!(reply.body.contains("HTTP/1.1 401 Unauthorized"));
    assert!(reply.body.contains("{\n  \"name\": \"object\"\n}"));
    assert!(reply.body.contains("{\n  \"metageneration\": \"1\"\n}"));
    assert!(reply.body.contains("Content-ID: response-get\r\n"));
    assert_eq!(reply.body.matches("HTTP/1.1 401 Unauthorized").count(), 2);
    let body = envelope(&[part(
        None,
        "GET",
        &format!("{BUCKET}/o/object"),
        "Authorization: Bearer admin\r\n",
        "",
    )]);
    server
        .request(
            "POST",
            "/batch/storage/v1",
            &format!("{TYPE}Authorization: Bearer invalid\r\n"),
            &body,
        )
        .status(200);
    let reply = server.request(
        "POST",
        "/batch/storage/v1?access_token=admin",
        &headers,
        &body,
    );
    reply.status(200);
    assert!(reply.body.contains("HTTP/1.1 401 Unauthorized"));
    server
        .request(
            "GET",
            &format!("{BUCKET}/o/object"),
            "Authorization: Bearer admin\r\n",
            b"",
        )
        .status(200);
}

#[test]
fn batch_validates_the_whole_envelope_before_mutating() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let first = part(None, "DELETE", &format!("{BUCKET}/o/object"), "", "");
    let invalid_parts = [
        part(
            None,
            "GET",
            "http://evil.invalid/storage/v1/b/batch-bucket",
            "",
            "",
        ),
        part(
            None,
            "GET",
            "//evil.invalid/storage/v1/b/batch-bucket",
            "",
            "",
        ),
        part(None, "GET", "/_internal/config", "", ""),
        part(None, "POST", "/batch/storage/v1", "", ""),
        part(
            None,
            "POST",
            "/upload/storage/v1/b/batch-bucket/o?uploadType=media&name=bad",
            "",
            "x",
        ),
        part(
            None,
            "GET",
            &format!("{BUCKET}/o/object?%61lt=media"),
            "",
            "",
        ),
        part(None, "GET", BUCKET, "Host: evil.invalid\r\n", ""),
        part(None, "PATCH", BUCKET, "Content-Length: 3\r\n", "{}"),
        part(
            None,
            "PATCH",
            BUCKET,
            "Content-Length: 2\r\nContent-Length: 2\r\n",
            "{}",
        ),
        part(None, "GET", BUCKET, "Transfer-Encoding: chunked\r\n", ""),
        part(
            None,
            "GET",
            BUCKET,
            "X-Good: value\r\n folded: invalid\r\n",
            "",
        ),
        part(None, "GET", BUCKET, "", "").replace(
            "Content-Transfer-Encoding: binary",
            "Content-Transfer-Encoding: base64",
        ),
        part(
            None,
            "GET",
            BUCKET,
            &format!("X-Long: {}\r\n", "x".repeat(8192)),
            "",
        ),
        part(None, "GET", BUCKET, "", "").replace("Content-Type: application/http\r\n", ""),
        part(None, "GET", BUCKET, "", "").replace(
            "Content-Type: application/http\r\n",
            "Content-Type: text/plain\r\n",
        ),
    ];
    for invalid in invalid_parts {
        let crlf = envelope(&[first.clone(), invalid]);
        for body in [lf(&crlf), crlf] {
            server
                .request("POST", "/batch/storage/v1", TYPE, &body)
                .status(400);
            server
                .request("GET", &format!("{BUCKET}/o/object"), "", b"")
                .status(200);
        }
    }
    let missing_close = first.as_bytes();
    for body in [lf(missing_close), missing_close.to_vec()] {
        server
            .request("POST", "/batch/storage/v1", TYPE, &body)
            .status(400);
        server
            .request("GET", &format!("{BUCKET}/o/object"), "", b"")
            .status(200);
    }
}

/// The framing variants recorded against Cloud Storage on 2026-11-09 (blank line kept, shared
/// with the delimiter's CRLF, and absent), in CRLF and in LF, plus a CRLF inner request wrapped in
/// LF MIME framing, whose delimiter line ending is LF.
#[test]
fn batch_deletes_under_each_recorded_line_ending_form() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let mime = "--batch-boundary\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\nContent-ID: <a>\r\n\r\n";
    let target = format!("{BUCKET}/o/object");
    let forms = [
        (
            "blank line then delimiter CRLF",
            "\r\n\r\n\r\n",
            "\r\n\r\n\n",
        ),
        (
            "headers sharing the delimiter CRLF",
            "\r\nAccept: application/json\r\n\r\n",
            "\r\nAccept: application/json\r\n\r\n",
        ),
        ("request line then delimiter", "\r\n", "\r\n"),
        (
            "headers then delimiter",
            "\r\nAccept: application/json\r\n",
            "\r\nAccept: application/json\r\n",
        ),
    ];
    for (name, tail, mixed_tail) in forms {
        let crlf = format!("{mime}DELETE {target} HTTP/1.1{tail}--batch-boundary--\r\n");
        let mixed = format!("DELETE {target} HTTP/1.1{mixed_tail}--batch-boundary--\n");
        for (framing, body) in [
            ("crlf", crlf.clone().into_bytes()),
            ("lf", lf(crlf.as_bytes())),
            (
                "lf mime around crlf request",
                [lf(mime.as_bytes()), mixed.into_bytes()].concat(),
            ),
        ] {
            upload(&server, "object");
            let reply = server.request("POST", "/batch/storage/v1", TYPE, &body);
            reply.status(200);
            assert_eq!(
                statuses(&reply.body),
                ["HTTP/1.1 204 No Content"],
                "{name}, {framing}: {}",
                reply.body
            );
            assert!(reply.body.contains("Content-Type: application/http\r\n"));
            assert!(reply.body.contains("Content-ID: <response-a>\r\n"));
            server.request("GET", &target, "", b"").status(404);
        }
    }
}

#[test]
fn batch_passes_part_bodies_intact_in_both_line_endings() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let object = format!("{BUCKET}/o/object");
    let declared = r#"{"metadata":{"key":"declared"}}"#;
    let undeclared = "{\r\n\r\n  \"metadata\": {\"other\": \"undeclared\"}\r\n}";
    let envelope = |bucket: &str| {
        envelope(&[
            part(
                Some("<patch>"),
                "PATCH",
                &object,
                &format!(
                    "Content-Type: application/json\r\nContent-Length: {}\r\n",
                    declared.len()
                ),
                declared,
            ),
            part(
                Some("<multiline>"),
                "PATCH",
                &object,
                "Content-Type: application/json\r\n",
                undeclared,
            ),
            part(
                Some("<create>"),
                "POST",
                "/storage/v1/b?project=project-a",
                "Content-Type: application/json\r\n",
                &format!(r#"{{"name":"{bucket}"}}"#),
            ),
        ])
    };
    for (bucket, body) in [
        ("created-crlf", envelope("created-crlf")),
        ("created-lf", lf(&envelope("created-lf"))),
    ] {
        let reply = server.request("POST", "/batch/storage/v1", TYPE, &body);
        reply.status(200);
        assert_eq!(
            statuses(&reply.body),
            ["HTTP/1.1 200 OK", "HTTP/1.1 200 OK", "HTTP/1.1 200 OK"],
            "{bucket}: {}",
            reply.body
        );
        let metadata = server.request("GET", &object, "", b"").json();
        assert_eq!(metadata["metadata"]["key"], "declared", "{bucket}");
        assert_eq!(metadata["metadata"]["other"], "undeclared", "{bucket}");
        server
            .request("GET", &format!("/storage/v1/b/{bucket}"), "", b"")
            .status(200);
    }
}

#[test]
fn batch_recognizes_delimiters_only_at_line_starts() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let object = format!("{BUCKET}/o/object");
    let value = "x--batch-boundary--y--batch-boundary";
    let mid_line = envelope(&[part(
        None,
        "PATCH",
        &object,
        "Content-Type: application/json\r\n",
        &format!(r#"{{"metadata":{{"key":"{value}"}}}}"#),
    )]);
    // A preamble line that merely contains the delimiter is not the opening delimiter.
    let preamble = [
        b"preamble x--batch-boundary\r\n".as_slice(),
        mid_line.as_slice(),
    ]
    .concat();
    for body in [lf(&mid_line), lf(&preamble), mid_line.clone(), preamble] {
        let reply = server.request("POST", "/batch/storage/v1", TYPE, &body);
        reply.status(200);
        assert_eq!(statuses(&reply.body), ["HTTP/1.1 200 OK"], "{}", reply.body);
        assert_eq!(
            server.request("GET", &object, "", b"").json()["metadata"]["key"],
            value
        );
    }
    // In a CRLF envelope an LF-only delimiter line is body content: nothing after it runs as a
    // part, and Content-Length sees the whole body.
    let hidden = format!(
        "{{\"metadata\":{{\"key\":\"hidden\"}}}}\n--batch-boundary\nContent-Type: application/http\n\nDELETE {object} HTTP/1.1\n\n"
    );
    let body = envelope(&[part(
        None,
        "PATCH",
        &object,
        &format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            hidden.len()
        ),
        &hidden,
    )]);
    let reply = server.request("POST", "/batch/storage/v1", TYPE, &body);
    reply.status(200);
    assert_eq!(
        statuses(&reply.body),
        ["HTTP/1.1 400 Bad Request"],
        "{}",
        reply.body
    );
    let kept = server.request("GET", &object, "", b"");
    kept.status(200);
    assert_eq!(kept.json()["metadata"]["key"], value);
    // A later CRLF envelope delimiter given as LF only is not one, so the closing never arrives.
    let lf_close = format!(
        "{}\n--batch-boundary--\n",
        part(None, "GET", &object, "", "").trim_end_matches("\r\n")
    );
    server
        .request("POST", "/batch/storage/v1", TYPE, lf_close.as_bytes())
        .status(400);
}

/// JSON tolerates trailing whitespace, so a matching Content-Length proves which bytes of a body
/// survived: only the delimiter's own line ending may be removed.
#[test]
fn batch_keeps_body_bytes_up_to_the_delimiter_line_ending() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let object = format!("{BUCKET}/o/object");
    let patch = |key: &str, tail: &str| {
        let body = format!(r#"{{"metadata":{{"key":"{key}"}}}}{tail}"#);
        let headers = format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
        (body, headers)
    };
    let cases: [(&str, &str, bool); 6] = [
        ("crlf-lf", "\n", false),
        ("crlf-cr", "\r", false),
        ("crlf-crlf", "\r\n", false),
        ("lf-lf", "\n", true),
        ("lf-cr", "\r", true),
        ("lf-crlf", "\r\n", true),
    ];
    for (key, tail, lf_framed) in cases {
        let (body, headers) = patch(key, tail);
        let framed = if lf_framed {
            let head = part(None, "PATCH", &object, &headers, "").replace("\r\n", "\n");
            let head = head.strip_suffix('\n').unwrap();
            format!("{head}{body}\n--batch-boundary--\n").into_bytes()
        } else {
            envelope(&[part(None, "PATCH", &object, &headers, &body)])
        };
        let reply = server.request("POST", "/batch/storage/v1", TYPE, &framed);
        reply.status(200);
        assert_eq!(
            statuses(&reply.body),
            ["HTTP/1.1 200 OK"],
            "{key}: {}",
            reply.body
        );
        assert_eq!(
            server.request("GET", &object, "", b"").json()["metadata"]["key"],
            key
        );
    }
}

#[test]
fn batch_line_ending_tolerance_keeps_limits_and_rejections() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let object = format!("{BUCKET}/o/object");
    let get = part(None, "GET", &format!("{object}?fields=name"), "", "");
    let check = |status: u16, body: &[u8]| {
        server
            .request("POST", "/batch/storage/v1", TYPE, body)
            .status(status);
    };

    // The 8 KiB inner header block holds whether the block ends with a blank line or the part.
    let padded = |filler: usize| format!("X-Long: {}\r\n", "x".repeat(filler));
    for (filler, status) in [(8184, 200), (8185, 400)] {
        let full = envelope(&[part(None, "GET", BUCKET, &padded(filler), "")]);
        check(status, &full);
        check(status, &lf(&full));
        let at_delimiter = format!(
            "--batch-boundary\r\nContent-Type: application/http\r\n\r\nGET {BUCKET} HTTP/1.1\r\n{}--batch-boundary--\r\n",
            padded(filler)
        );
        check(status, at_delimiter.as_bytes());
        check(status, &lf(at_delimiter.as_bytes()));
    }
    // The MIME block has the same limit, which LF lines reach with a longer value because the
    // block holds 75 other bytes in CRLF and 73 in LF. Without a blank line it holds no request.
    let mime = |filler: usize| {
        envelope(&[get.replace(
            "Content-Type: application/http\r\n",
            &format!("Content-Type: application/http\r\n{}", padded(filler)),
        )])
    };
    for (filler, status) in [(8117, 200), (8118, 400)] {
        check(status, &mime(filler));
    }
    for (filler, status) in [(8119, 200), (8120, 400)] {
        check(status, &lf(&mime(filler)));
    }
    let no_request =
        b"--batch-boundary\r\nContent-Type: application/http\r\n--batch-boundary--\r\n";
    check(400, no_request);
    check(400, &lf(no_request));

    let headers = |count: usize| {
        (0..count)
            .map(|index| format!("X-Header-{index}: value\r\n"))
            .collect::<String>()
    };
    for (count, status) in [(64, 200), (65, 400)] {
        let body = envelope(&[part(None, "GET", BUCKET, &headers(count), "")]);
        check(status, &lf(&body));
    }

    let single = envelope(std::slice::from_ref(&get));
    let preamble = |length: usize| format!("{}\n", "x".repeat(length - 1)).into_bytes();
    for (length, status) in [(4096, 200), (4097, 400)] {
        check(status, &[preamble(length), lf(&single)].concat());
        check(status, &[lf(&single), vec![b'x'; length]].concat());
    }

    let hundred = lf(&envelope(&vec![get.clone(); 100]));
    let reply = server.request("POST", "/batch/storage/v1", TYPE, &hundred);
    reply.status(200);
    assert_eq!(reply.body.matches("HTTP/1.1 200 OK").count(), 100);
    check(400, &lf(&envelope(&vec![get.clone(); 101])));

    let delete = part(None, "DELETE", &object, "", "");
    let framed = String::from_utf8(lf(delete.as_bytes())).unwrap();
    for malformed in [
        // Delimiter lines that only begin like one.
        format!("{framed}--batch-boundaryx\n"),
        format!("{framed}--batch-boundary--x\n"),
        format!("{framed}--batch-boundary-"),
        format!("{framed}--batch-boundary\r"),
        // No opening delimiter at a line start, or one that is cut short.
        format!("x{framed}--batch-boundary--\n"),
        "--batch-boundary\rContent-Type: application/http\n\n".to_owned(),
        // Parts without a usable request line.
        "--batch-boundary\nContent-Type: application/http\n\n\n--batch-boundary--\n".to_owned(),
        format!(
            "--batch-boundary\nContent-Type: application/http\n\nDELETE {object}\n\n\n--batch-boundary--\n"
        ),
        format!(
            "--batch-boundary\nContent-Type: application/http\n\nDELETE {object} HTTP/1.0\n\n\n--batch-boundary--\n"
        ),
        // A MIME block whose Content-Type is not application/http.
        format!(
            "--batch-boundary\nContent-ID: <a>\n\nDELETE {object} HTTP/1.1\n\n\n--batch-boundary--\n"
        ),
        // A Content-Length that the empty body at the delimiter does not satisfy.
        format!(
            "--batch-boundary\nContent-Type: application/http\n\nPATCH {object} HTTP/1.1\nContent-Length: 2\n--batch-boundary--\n"
        ),
    ] {
        check(400, malformed.as_bytes());
    }
    server.request("GET", &object, "", b"").status(200);
}

#[test]
fn batch_bounds_part_count_total_bytes_and_mime_framing() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    setup(&server, "");
    let get = part(
        None,
        "GET",
        &format!("{BUCKET}/o/object?fields=name"),
        "",
        "",
    );
    let hundred = envelope(&vec![get.clone(); 100]);
    let reply = server.request("POST", "/batch/storage/v1", TYPE, &hundred);
    reply.status(200);
    assert_eq!(reply.body.matches("HTTP/1.1 200 OK").count(), 100);
    server
        .request(
            "POST",
            "/batch/storage/v1",
            &format!("{TYPE}Content-Encoding: gzip\r\n"),
            &common::ingress::gzip(&hundred),
        )
        .status(400);
    server
        .request(
            "POST",
            "/batch/storage/v1",
            TYPE,
            &envelope(&vec![get.clone(); 101]),
        )
        .status(400);
    for headers in [
        "Content-Type: multipart/mixed\r\n",
        "Content-Type: multipart/mixed; boundary=x; boundary=y\r\n",
        "Content-Type: multipart/mixed; boundary=\"x \"\r\n",
        "Content-Type: multipart/mixed; boundary=batch-boundary\r\nContent-Type: multipart/mixed; boundary=batch-boundary\r\n",
    ] {
        server
            .request("POST", "/batch/storage/v1", headers, &hundred)
            .status(400);
    }
    for length in [9_999_999, 10_000_000, 10_000_001] {
        server
            .request("POST", "/batch/storage/v1", TYPE, &vec![b'x'; length])
            .status(if length < 10_000_000 { 400 } else { 413 });
    }
    let padded = String::from_utf8(envelope(&[get]))
        .unwrap()
        .replace("--batch-boundary\r\n", "--batch-boundary \t\r\n")
        .replace("--batch-boundary--\r\n", "--batch-boundary-- \t\r\n");
    server
        .request("POST", "/batch/storage/v1", TYPE, padded.as_bytes())
        .status(200);
    // Only the envelope's documented limit bounds a part's body.
    let mut large = "{}".to_string();
    large.extend(std::iter::repeat_n(' ', 9_000_000));
    let body = envelope(&[part(
        None,
        "PATCH",
        BUCKET,
        "Content-Type: application/json\r\n",
        &large,
    )]);
    let reply = server.request("POST", "/batch/storage/v1", TYPE, &body);
    reply.status(200);
    assert_eq!(statuses(&reply.body), ["HTTP/1.1 200 OK"]);
    for body in [
        b"--batch-boundary--\r\n".to_vec(),
        hundred[..hundred.len() - 3].to_vec(),
        [hundred.as_slice(), &vec![b'x'; 4097]].concat(),
    ] {
        server
            .request("POST", "/batch/storage/v1", TYPE, &body)
            .status(400);
    }
}

fn statuses(body: &str) -> Vec<&str> {
    body.lines()
        .filter(|line| line.starts_with("HTTP/1.1 "))
        .collect()
}

/// A full object list exceeds the former 512 KiB per part, and a few of them the former 10 MB in
/// all. Both used to answer 413 for a call that had succeeded.
#[test]
fn batch_answers_full_object_lists_past_the_former_response_limits() {
    const OBJECTS: usize = 1000;
    const LISTS: usize = 10;
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--backend", "memory"]);
    setup(&server, "");
    for index in 0..OBJECTS - 2 {
        let name = format!("{index:04}{}", "n".repeat(300));
        server
            .request(
                "POST",
                &format!("/upload/storage/v1/b/batch-bucket/o?uploadType=media&name={name}"),
                "",
                b"",
            )
            .status(200);
    }
    let list = part(
        None,
        "GET",
        &format!("{BUCKET}/o?maxResults={OBJECTS}"),
        "",
        "",
    );
    let reply = server.request(
        "POST",
        "/batch/storage/v1",
        TYPE,
        &envelope(&vec![list; LISTS]),
    );
    reply.status(200);
    assert_eq!(statuses(&reply.body), vec!["HTTP/1.1 200 OK"; LISTS]);
    assert!(reply.body.len() > 10_000_000, "{}", reply.body.len());
    assert!(reply.headers.contains("transfer-encoding: chunked"));
    assert!(reply.body.ends_with("--gcs_mimic_batch_response--\r\n"));
    for response in reply
        .body
        .split("--gcs_mimic_batch_response\r\n")
        .filter(|section| !section.is_empty())
    {
        let (_, body) = response.split_once("\r\n\r\nHTTP/1.1 ").unwrap();
        let (head, body) = body.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("200 OK\r\n"), "{head}");
        let body = body.trim_end_matches("--gcs_mimic_batch_response--\r\n");
        let page: serde_json::Value = serde_json::from_str(body.trim_end()).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), OBJECTS);
    }
}

#[test]
fn batch_accepts_absolute_targets_only_on_the_advertised_api_origin() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--api-base-url", "http://gcs.invalid:4443"]);
    setup(&server, "");
    // The Java client writes absolute request lines built from its configured endpoint, and
    // Google answers them like relative paths; equivalent spellings of the origin must match.
    for origin in [
        "http://gcs.invalid:4443",
        "HTTP://GCS.Invalid:4443",
        "http://GCS.INVALID:4443",
    ] {
        let reply = server.request(
            "POST",
            "/batch/storage/v1",
            TYPE,
            &envelope(&[
                part(
                    Some("abs"),
                    "GET",
                    &format!("{origin}{BUCKET}/o/object?fields=name"),
                    "",
                    "",
                ),
                part(Some("rel"), "GET", &format!("{BUCKET}/o/missing"), "", ""),
            ]),
        );
        reply.status(200);
        assert_eq!(
            statuses(&reply.body),
            ["HTTP/1.1 200 OK", "HTTP/1.1 404 Not Found"]
        );
        assert!(reply.body.contains("\"object\""));
    }
    // A foreign origin is never forwarded, and the whole envelope fails before any deletion.
    let first = part(None, "DELETE", &format!("{BUCKET}/o/object"), "", "");
    for target in [
        "http://other.invalid:4443/storage/v1/b/batch-bucket",
        "https://gcs.invalid:4443/storage/v1/b/batch-bucket",
        "http://gcs.invalid:4444/storage/v1/b/batch-bucket",
        "http://gcs.invalid/storage/v1/b/batch-bucket",
        "http://127.0.0.1:4443/storage/v1/b/batch-bucket",
        "http://user@gcs.invalid:4443/storage/v1/b/batch-bucket",
        "http://user:secret@gcs.invalid:4443/storage/v1/b/batch-bucket",
        "http://gcs.invalid:4443@other.invalid/storage/v1/b/batch-bucket",
        "ftp://gcs.invalid:4443/storage/v1/b/batch-bucket",
        "http://gcs.invalid:4443",
        "http://gcs.invalid:4443/_internal/config",
        "http://gcs.invalid:4443/batch/storage/v1",
        "http://gcs.invalid:4443/upload/storage/v1/b/batch-bucket/o?uploadType=media&name=x",
        "http://gcs.invalid:4443/storage/v1/b/batch-bucket/o/object?alt=media",
        "http://gcs.invalid:4443//storage/v1/b/batch-bucket",
        "http://gcs.invalid:4443/storage/v1/b/batch-bucket#fragment",
    ] {
        server
            .request(
                "POST",
                "/batch/storage/v1",
                TYPE,
                &envelope(&[first.clone(), part(None, "GET", target, "", "")]),
            )
            .status(400);
        server
            .request("GET", &format!("{BUCKET}/o/object"), "", b"")
            .status(200);
    }
}

#[test]
fn batch_absolute_targets_include_the_advertised_path_prefix() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--api-base-url", "https://proxy.invalid/gcs/"]);
    setup(&server, "");
    let reply = server.request(
        "POST",
        "/batch/storage/v1",
        TYPE,
        &envelope(&[part(
            None,
            "GET",
            &format!("https://proxy.invalid:443/gcs{BUCKET}/o/object?fields=name"),
            "",
            "",
        )]),
    );
    reply.status(200);
    assert_eq!(statuses(&reply.body), ["HTTP/1.1 200 OK"]);
    for target in [
        format!("https://proxy.invalid{BUCKET}"),
        format!("https://proxy.invalid/gcsx{BUCKET}"),
        format!("https://proxy.invalid/other/gcs{BUCKET}"),
    ] {
        server
            .request(
                "POST",
                "/batch/storage/v1",
                TYPE,
                &envelope(&[part(None, "GET", &target, "", "")]),
            )
            .status(400);
    }
}
