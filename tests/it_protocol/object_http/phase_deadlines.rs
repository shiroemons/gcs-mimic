//! Each connection phase has its own deadline: a slow client mid-transfer is not a stuck request.
//! Short explicit limits keep real time low; no test waits for a default.
use super::*;

/// Longer than `LIMIT` by enough to survive scheduler stalls on loaded runners.
const STALL: Duration = Duration::from_millis(1500);
const LIMIT: &str = "500";

/// Holds for `wait` and proves that the server neither answered nor closed meanwhile.
fn assert_silent(stream: &TcpStream, wait: Duration) {
    stream.set_read_timeout(Some(wait)).unwrap();
    let error = stream
        .peek(&mut [0])
        .expect_err("the server answered or closed during the stall");
    assert!(
        matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
        "{error}"
    );
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
}

fn start_with(dir: &TestDir, args: &[&str]) -> Server {
    Server::launch(http_command(&dir.0).args(args))
}

/// A server whose header and request deadlines are far shorter than the client's stall, with
/// body inactivity left at its default (disabled).
fn impatient(dir: &TestDir) -> Server {
    before_short_deadline(dir, |_| ());
    start_with(
        dir,
        &[
            "--header-read-timeout-ms",
            LIMIT,
            "--request-timeout-ms",
            LIMIT,
        ],
    )
}

/// Sends `head`, then each part after a stall.
fn stalled(server: &Server, head: &str, parts: &[&[u8]]) -> Reply {
    let mut stream = server.stream();
    stream.set_nodelay(true).unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    for part in parts {
        assert_silent(&stream, STALL);
        stream.write_all(part).unwrap();
    }
    Reply::framed(stream)
}

#[test]
fn obj_http_stalled_upload_body_outlives_header_and_request_deadlines() {
    let dir = TestDir::new();
    let server = impatient(&dir);

    // Before the first body byte as well as between later ones.
    stalled(
        &server,
        "POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=media HTTP/1.1\r\nHost: local\r\nConnection: close\r\nContent-Length: 10\r\n\r\n",
        &[b"01234", b"56789"],
    )
    .expect(200);
    assert_eq!(server.get("media?alt=media").body, b"0123456789");

    stalled(
        &server,
        "POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=chunked HTTP/1.1\r\nHost: local\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n",
        &[b"5\r\n01234\r\n", b"5\r\n56789\r\n0\r\n\r\n"],
    )
    .expect(200);
    assert_eq!(server.get("chunked?alt=media").body, b"0123456789");

    let multipart = b"--b\r\nContent-Type: application/json\r\n\r\n{\"name\":\"multipart\"}\r\n--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b--\r\n";
    let (first, second) = multipart.split_at(50);
    stalled(
        &server,
        &format!(
            "POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: local\r\nConnection: close\r\nContent-Type: multipart/related; boundary=b\r\nContent-Length: {}\r\n\r\n",
            multipart.len()
        ),
        &[first, second],
    )
    .expect(200);
    assert_eq!(server.get("multipart?alt=media").body, b"hello");

    stalled(
        &server,
        "PUT /test-bucket/xml HTTP/1.1\r\nHost: local\r\nConnection: close\r\nContent-Length: 10\r\n\r\n",
        &[b"01234", b"56789"],
    )
    .expect(200);
    assert_eq!(server.get("xml?alt=media").body, b"0123456789");
}

#[test]
fn obj_http_stalled_resumable_chunk_outlives_header_and_request_deadlines() {
    let dir = TestDir::new();
    let server = impatient(&dir);
    let started = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=session",
        "Content-Type: application/json\r\n",
        b"{}",
    );
    started.expect(200);
    let location = started.headers["location"].clone();
    let path = location
        .find("/upload/storage/")
        .map(|start| &location[start..])
        .unwrap();
    stalled(
        &server,
        &format!(
            "PUT {path} HTTP/1.1\r\nHost: local\r\nConnection: close\r\nContent-Length: 10\r\nContent-Range: bytes 0-9/10\r\n\r\n"
        ),
        &[b"01234", b"56789"],
    )
    .expect(200);
    assert_eq!(server.get("session?alt=media").body, b"0123456789");
}

/// A resource body may reach 16 MiB, so a pause inside it is the client's pace on every route:
/// the request deadline measures the server and never ends it.
#[test]
fn obj_http_stalled_resource_bodies_outlive_header_and_request_deadlines() {
    let dir = TestDir::new();
    let server = impatient(&dir);
    server.upload("name=described", "", b"data").expect(200);

    // Each request sends its head and the start of its body, then stalls mid-body.
    let mid_body = |head: &str, body: &str, split: usize| {
        let (first, second) = body.split_at(split);
        let head = format!(
            "{head}Connection: close\r\nContent-Length: {}\r\n\r\n{first}",
            body.len()
        );
        stalled(&server, &head, &[second.as_bytes()])
    };
    mid_body(
        "PATCH /storage/v1/b/test-bucket/o/described HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\n",
        r#"{"metadata":{"stalled":"yes"}}"#,
        12,
    )
    .expect(200);
    assert_eq!(server.get("described").json()["metadata"]["stalled"], "yes");

    // A bucket creation spends the request deadline on its catalog write too, so a loaded runner
    // can answer it 503 after the body arrived. The stall cannot cause that, because `stalled`
    // proves the server stayed silent through it. Such an attempt is retried under a fresh name,
    // since its dropped write may still commit.
    let mut attempts = 0;
    let mut created_after_stall =
        |prefix: &str, request: fn(&str) -> (String, String), split: usize| {
            common::input_fixtures::eventually_some(
                "a stalled bucket creation answered in time",
                || {
                    attempts += 1;
                    let name = format!("{prefix}-{attempts}");
                    let (head, body) = request(&name);
                    let reply = mid_body(&head, &body, split);
                    (reply.status != 503).then_some((name, reply))
                },
            )
        };
    let (name, reply) = created_after_stall(
        "stalled-bucket",
        |name| {
            (
                "POST /storage/v1/b?project=local HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\n".to_owned(),
                format!(r#"{{"name":"{name}"}}"#),
            )
        },
        9,
    );
    reply.expect(200);
    server
        .request("GET", &format!("/storage/v1/b/{name}"), "", b"")
        .expect(200);

    let (name, reply) = created_after_stall(
        "stalled-xml-bucket",
        |name| {
            (
                format!("PUT /{name} HTTP/1.1\r\nHost: local\r\nx-goog-project-id: local\r\n"),
                "<CreateBucketConfiguration><StorageClass>NEARLINE</StorageClass></CreateBucketConfiguration>".to_owned(),
            )
        },
        20,
    );
    reply.expect(200);
    let created = server.request("GET", &format!("/storage/v1/b/{name}"), "", b"");
    assert_eq!(created.json_ok()["storageClass"], "NEARLINE");
}

/// Body inactivity bounds a silent body on every route. Its expiry is the documented request
/// timeout of each API: 408 `uploadBrokenConnection` in JSON, 400 `RequestTimeout` in XML. Each
/// expiry also logs a warning, which is how an operator finds such a client.
#[test]
fn obj_http_explicit_body_inactivity_answers_silent_resource_bodies() {
    let dir = TestDir::new();
    before_short_deadline(&dir, |setup| {
        setup.upload("name=described", "", b"data").expect(200);
    });
    let mut server = Server::launch(
        http_command(&dir.0)
            .args(["--body-inactivity-timeout-ms", LIMIT])
            .stderr(std::process::Stdio::piped()),
    );
    let silent = |head: &str| {
        let mut stream = server.stream();
        stream.write_all(head.as_bytes()).unwrap();
        Reply::framed(stream)
    };
    let names_the_setting = |reply: &Reply| {
        assert!(
            String::from_utf8_lossy(&reply.body).contains("body_inactivity_timeout_ms"),
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(reply.headers["connection"], "close");
    };

    let patch = silent(
        "PATCH /storage/v1/b/test-bucket/o/described HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"metadata\":",
    );
    patch.json_error(408, "uploadBrokenConnection");
    names_the_setting(&patch);
    assert!(server.get("described").json().get("metadata").is_none());

    let bucket = silent(
        "POST /storage/v1/b?project=local HTTP/1.1\r\nHost: local\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"name\":",
    );
    bucket.json_error(408, "uploadBrokenConnection");
    names_the_setting(&bucket);

    let object = silent(
        "PUT /test-bucket/xml-silent HTTP/1.1\r\nHost: local\r\nContent-Length: 10\r\n\r\n01234",
    );
    common::errors::xml_error_with_content_type(&object, 400, "RequestTimeout");
    names_the_setting(&object);
    server.get("xml-silent").json_error(404, "notFound");

    let xml_bucket = silent(
        "PUT /silent-xml-bucket HTTP/1.1\r\nHost: local\r\nx-goog-project-id: local\r\nContent-Length: 100\r\n\r\n<CreateBucketConfiguration>",
    );
    common::errors::xml_error_with_content_type(&xml_bucket, 400, "RequestTimeout");
    names_the_setting(&xml_bucket);
    server
        .request("GET", "/storage/v1/b/silent-xml-bucket", "", b"")
        .json_error(404, "notFound");

    server.stop();
    let mut stderr = String::new();
    server
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let warnings = stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            event["event"] == "connection_timeout"
                && event["level"] == "warn"
                && event["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("body_inactivity_timeout_ms"))
        })
        .count();
    assert_eq!(warnings, 4, "{stderr}");
}

/// Every frame of a resource body renews body inactivity, as it does for an upload, so a body
/// that trickles in for several times the limit still completes.
#[test]
fn obj_http_trickled_resource_body_renews_body_inactivity() {
    let dir = TestDir::new();
    before_short_deadline(&dir, |_| ());
    let server = start_with(
        &dir,
        &[
            "--body-inactivity-timeout-ms",
            LIMIT,
            "--request-timeout-ms",
            LIMIT,
        ],
    );
    let body = br#"{"name":"trickled-bucket"}"#;
    let mut stream = server.stream();
    stream.set_nodelay(true).unwrap();
    write!(stream, "POST /storage/v1/b?project=local HTTP/1.1\r\nHost: local\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
    // A sixth of the limit between bytes; the whole body takes about four limits.
    for byte in body {
        assert_silent(&stream, Duration::from_millis(80));
        stream.write_all(&[*byte]).unwrap();
    }
    Reply::framed(stream).expect(200);
    server
        .request("GET", "/storage/v1/b/trickled-bucket", "", b"")
        .expect(200);
}

/// More than the kernel socket buffers hold, so a client that stops reading stalls the response.
const LARGE: usize = 64 * 1024 * 1024;

fn large_object(dir: &TestDir) {
    let server = start_with(dir, &[]);
    server.bucket();
    let reply = server.upload("name=large", "", &vec![b'x'; LARGE]);
    reply.expect(200);
    drop(server);
}

fn stalled_download(server: &Server) -> TcpStream {
    let mut stream = server.stream();
    stream
        .write_all(b"GET /storage/v1/b/test-bucket/o/large?alt=media HTTP/1.1\r\nHost: local\r\nConnection: close\r\n\r\n")
        .unwrap();
    stream
}

fn read_all(stream: &mut TcpStream) -> usize {
    let mut received = 0;
    let mut buffer = vec![0; 1024 * 1024];
    while let Ok(1..) = stream.read(&mut buffer).inspect(|count| received += *count) {}
    received
}

/// Holds for a stall while a download's client reads nothing. Its own socket has data to read by
/// then, so a connection idle between requests, which these limits do not cover, keeps the time.
fn hold_during_download(server: &Server) {
    let mut idle = server.stream();
    idle.write_all(b"GET /_internal/health HTTP/1.1\r\nHost: local\r\n\r\n")
        .unwrap();
    let mut idle = BufReader::new(idle);
    common::read_http_response(&mut idle, common::http::Body::Expected).unwrap();
    assert_silent(idle.get_ref(), STALL);
}

#[test]
fn obj_http_stalled_download_outlives_header_and_request_deadlines() {
    let dir = TestDir::new();
    large_object(&dir);
    let server = start_with(
        &dir,
        &[
            "--header-read-timeout-ms",
            LIMIT,
            "--request-timeout-ms",
            LIMIT,
        ],
    );
    let mut stream = stalled_download(&server);
    hold_during_download(&server);
    assert!(read_all(&mut stream) > LARGE, "the response was cut short");
}

#[test]
fn obj_http_explicit_body_inactivity_cuts_a_stalled_download() {
    let dir = TestDir::new();
    large_object(&dir);
    let server = start_with(&dir, &["--body-inactivity-timeout-ms", LIMIT]);
    let mut stream = stalled_download(&server);
    hold_during_download(&server);
    assert!(read_all(&mut stream) < LARGE, "the stall went unnoticed");
}

#[test]
fn obj_http_explicit_body_inactivity_answers_a_stalled_upload_with_408() {
    let dir = TestDir::new();
    before_short_deadline(&dir, |_| ());
    let server = start_with(&dir, &["--body-inactivity-timeout-ms", LIMIT]);
    let mut stream = server.stream();
    stream.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=idle HTTP/1.1\r\nHost: local\r\nContent-Length: 100\r\n\r\nx").unwrap();
    let reply = Reply::framed(stream);
    reply.json_error(408, "uploadBrokenConnection");
    assert!(
        reply.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("body_inactivity_timeout_ms"),
        "{}",
        reply.json()
    );
    server.get("idle").json_error(404, "notFound");
}

/// Zero disables, rather than expires at once, and the largest value must not overflow time
/// arithmetic, on every path that turns a setting into a deadline.
#[test]
fn obj_http_disabled_and_largest_timeouts_never_cut_any_phase() {
    for value in ["0", "18446744073709551615"] {
        let dir = TestDir::new();
        let mut server = start_with(
            &dir,
            &[
                "--header-read-timeout-ms",
                value,
                "--body-inactivity-timeout-ms",
                value,
                "--request-timeout-ms",
                value,
                "--keep-alive-timeout-ms",
                value,
                "--object-request-timeout-ms",
                value,
                "--catalog-wait-timeout-ms",
                value,
                "--shutdown-timeout-ms",
                value,
            ],
        );
        server.bucket();
        server.upload("name=source", "", b"hello").expect(200);
        assert_eq!(server.get("source?alt=media").body, b"hello");
        server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/copied",
                "",
                b"",
            )
            .expect(200);
        server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/rewritten",
                "",
                b"",
            )
            .expect(200);
        server
            .request(
                "POST",
                "/storage/v1/b/test-bucket/o/composed/compose",
                "Content-Type: application/json\r\n",
                br#"{"sourceObjects":[{"name":"source"}]}"#,
            )
            .expect(200);
        let started = server.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=session",
            "Content-Type: application/json\r\n",
            b"{}",
        );
        started.expect(200);
        let location = started.headers["location"].clone();
        let path = location
            .find("/upload/storage/")
            .map(|start| &location[start..])
            .unwrap();
        server
            .request("PUT", path, "Content-Range: bytes 0-4/5\r\n", b"hello")
            .expect(200);
        server
            .request("PUT", "/test-bucket/xml", "", b"hello")
            .expect(200);
        server
            .request("DELETE", "/storage/v1/b/test-bucket/o/copied", "", b"")
            .expect(204);

        // A connection that has not sent anything yet, and one idle between requests.
        let mut silent = BufReader::new(server.stream());
        let mut persistent = BufReader::new(server.stream());
        let health = b"GET /_internal/health HTTP/1.1\r\nHost: local\r\n\r\n";
        persistent.get_mut().write_all(health).unwrap();
        common::read_http_response(&mut persistent, common::http::Body::Expected).unwrap();
        for connection in [&mut silent, &mut persistent] {
            assert_silent(connection.get_ref(), Duration::from_millis(200));
            connection.get_mut().write_all(health).unwrap();
            let response =
                common::read_http_response(connection, common::http::Body::Expected).unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200"), "with {value}");
        }
        // Stopping still ends idle connections, even without a drain limit.
        server.stop();
    }
}

#[test]
fn obj_http_ready_event_reports_each_request_phase_timeout() {
    let dir = TestDir::new();
    let server = start_with(
        &dir,
        &[
            "--header-read-timeout-ms",
            "7000",
            "--body-inactivity-timeout-ms",
            "8000",
            "--request-timeout-ms",
            "9000",
        ],
    );
    assert_eq!(server.ready["header_read_timeout_ms"], 7000);
    assert_eq!(server.ready["body_inactivity_timeout_ms"], 8000);
    assert_eq!(server.ready["request_timeout_ms"], 9000);
}
