mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::Server;

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
    assert!(reply.headers.contains("multipart/mixed; boundary="));
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
    let registry = dir.0.join("identities.json");
    std::fs::write(&registry, serde_json::json!({"identities":[
        {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}},
        {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
    ]}).to_string()).unwrap();
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
    assert!(reply.body.contains(r#"{"name":"object"}"#));
    assert!(reply.body.contains(r#"{"metageneration":"1"}"#));
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
            "PATCH",
            BUCKET,
            "Content-Type: application/json\r\n",
            &" ".repeat(65537),
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
        server
            .request(
                "POST",
                "/batch/storage/v1",
                TYPE,
                &envelope(&[first.clone(), invalid]),
            )
            .status(400);
        server
            .request("GET", &format!("{BUCKET}/o/object"), "", b"")
            .status(200);
    }
    let missing_close = first.as_bytes();
    server
        .request("POST", "/batch/storage/v1", TYPE, missing_close)
        .status(400);
    server
        .request("GET", &format!("{BUCKET}/o/object"), "", b"")
        .status(200);
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
            &ingress_support::gzip(&hundred),
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
    let mut exact = "{}".to_string();
    exact.extend(std::iter::repeat_n(' ', 65534));
    let body = envelope(&[part(
        None,
        "PATCH",
        BUCKET,
        "Content-Type: application/json\r\n",
        &exact,
    )]);
    server
        .request("POST", "/batch/storage/v1", TYPE, &body)
        .status(200);
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
