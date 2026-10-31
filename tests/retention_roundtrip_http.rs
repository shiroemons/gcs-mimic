mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir) -> Self {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(dir.0.join("data"))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready event");
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
        }
    }

    fn request(&self, method: &str, path: &str, body: &str) -> (u16, Value) {
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: local.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let reply = common::read_http_response(&mut reader, false).unwrap();
        let reply = String::from_utf8(reply).unwrap();
        let (headers, body) = reply.split_once("\r\n\r\n").unwrap();
        if headers.contains("\r\nconnection: close") {
            common::await_server_close(&mut reader);
        }
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }

    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A time with a sub-millisecond fraction, far enough ahead to stay valid and active.
const FINE: &str = "2099-01-02T03:04:05.123456789Z";

fn object_path(name: &str) -> String {
    format!("/storage/v1/b/rt-bucket/o/{name}")
}

fn setup(server: &Server, names: &[&str]) {
    let (status, body) = server.request(
        "POST",
        "/storage/v1/b?project=local&enableObjectRetention=true",
        r#"{"name":"rt-bucket"}"#,
    );
    assert_eq!(status, 200, "{body}");
    for name in names {
        let (status, body) = server.request(
            "POST",
            &format!("/upload/storage/v1/b/rt-bucket/o?uploadType=media&name={name}"),
            "payload",
        );
        assert_eq!(status, 200, "{body}");
    }
}

fn set_retention(server: &Server, name: &str, mode: &str) -> Value {
    let (status, body) = server.request(
        "PATCH",
        &object_path(name),
        &json!({"retention":{"mode":mode,"retainUntilTime":FINE}}).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    body
}

/// Observed on Cloud Storage (2026-10-31): a Locked retention refuses shortening, removal and a
/// mode change with 403 `forbidden` and its own message, with or without the override. The
/// displayed millisecond value is a shortening, the same time again is accepted (and counts as a
/// metadata write), and a time a nanosecond later extends it.
#[test]
fn locked_retention_refuses_every_reduction_with_403_and_accepts_equal_or_later() {
    const LOCKED_MESSAGE: &str = "The locked object retention cannot be removed. Its retention mode cannot be changed and its retention period cannot be shortened.";
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["locked"]);
    let stored = set_retention(&server, "locked", "Locked");
    assert_eq!(
        stored["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    let patch = |server: &Server, body: Value, query: &str| {
        server.request(
            "PATCH",
            &format!("{}{query}", object_path("locked")),
            &body.to_string(),
        )
    };
    let locked = |time: &str| json!({"retention":{"mode":"Locked","retainUntilTime":time}});
    for query in ["", "?overrideUnlockedRetention=true"] {
        for body in [
            locked("2099-01-02T03:04:05.123Z"),
            locked("2099-01-02T03:04:05.123456788Z"),
            locked("2099-01-02T03:03:35.123456789Z"),
            json!({"retention":null}),
            json!({"retention":{"mode":"Unlocked","retainUntilTime":FINE}}),
        ] {
            let (status, reply) = patch(&server, body.clone(), query);
            assert_eq!(status, 403, "{body} {query} {reply}");
            assert_eq!(reply["error"]["errors"][0]["reason"], "forbidden");
            assert_eq!(reply["error"]["message"], LOCKED_MESSAGE);
        }
    }
    let (status, same) = patch(&server, locked(FINE), "");
    assert_eq!(status, 200, "{same}");
    let (status, later) = patch(&server, locked("2099-01-02T03:04:05.123456790Z"), "");
    assert_eq!(status, 200, "{later}");
    assert_eq!(
        later["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    // A restart keeps the nanoseconds, so the earlier time is now a shortening.
    server.stop();
    let mut server = Server::start(&dir);
    let (status, reply) = patch(&server, locked(FINE), "");
    assert_eq!(status, 403, "{reply}");
    server.stop();
}

/// Locking an Unlocked retention needs the override, with the Unlocked wording (observed).
#[test]
fn locking_an_unlocked_retention_needs_the_override() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["lock"]);
    set_retention(&server, "lock", "Unlocked");
    let body = json!({"retention":{"mode":"Locked","retainUntilTime":FINE}}).to_string();
    let (status, reply) = server.request("PATCH", &object_path("lock"), &body);
    assert_eq!(status, 403, "{reply}");
    assert_eq!(reply["error"]["errors"][0]["reason"], "forbidden");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("The unlocked object retention cannot be removed")
    );
    let (status, reply) = server.request(
        "PATCH",
        &format!("{}?overrideUnlockedRetention=true", object_path("lock")),
        &body,
    );
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["retention"]["mode"], "Locked");
    server.stop();
}

/// Cloud Storage kept the nanosecond value of an Unlocked time for comparisons while showing
/// milliseconds (observed 2026-10-31): a time one nanosecond earlier and the displayed
/// millisecond time were refused with 403, the same time and a later one were accepted.
/// gcs-mimic compares at the same precision, also after a restart.
#[test]
fn unlocked_retention_compares_below_the_displayed_millisecond() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["unlocked"]);
    let stored = set_retention(&server, "unlocked", "Unlocked");
    assert_eq!(
        stored["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    let send = |server: &Server, time: &str, query: &str| {
        server.request(
            "PATCH",
            &format!("{}{query}", object_path("unlocked")),
            &json!({"retention":{"mode":"Unlocked","retainUntilTime":time}}).to_string(),
        )
    };
    let refused = |server: &Server, time: &str| {
        let (status, body) = send(server, time, "");
        assert_eq!(status, 403, "{time} {body}");
        assert_eq!(body["error"]["errors"][0]["reason"], "forbidden");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("The unlocked object retention cannot be removed")
        );
    };
    for time in [
        "2099-01-02T03:04:05.123456788Z",
        "2099-01-02T03:04:05.123456Z",
        "2099-01-02T03:04:05.123Z",
    ] {
        refused(&server, time);
    }
    // A restart keeps the nanosecond remainder.
    server.stop();
    let mut server = Server::start(&dir);
    refused(&server, "2099-01-02T03:04:05.123456788Z");
    let (status, body) = send(&server, FINE, "");
    assert_eq!(status, 200, "{body}");
    // One nanosecond later is a real extension; the display does not change.
    let (status, body) = send(&server, "2099-01-02T03:04:05.123456790Z", "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );
    // The new time is now the floor: the original FINE time is a shortening.
    refused(&server, FINE);
    let (status, body) = send(
        &server,
        "2099-01-02T03:04:05.123Z",
        "?overrideUnlockedRetention=true",
    );
    assert_eq!(status, 200, "{body}");
    server.stop();
}

#[test]
fn custom_time_read_back_is_accepted_and_a_decrease_is_not() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["custom"]);
    let (status, body) = server.request(
        "PATCH",
        &object_path("custom"),
        &json!({"customTime":FINE}).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    for method in ["PATCH", "PUT"] {
        let (status, body) = server.request(
            method,
            &object_path("custom"),
            &json!({"customTime":body["customTime"]}).to_string(),
        );
        assert_eq!(status, 200, "{method} {body}");
    }
    let (status, body) = server.request(
        "PATCH",
        &object_path("custom"),
        r#"{"customTime":"2099-01-02T03:04:05.123456788Z"}"#,
    );
    assert_eq!(status, 400, "{body}");
    server.stop();
}

/// Observed on Cloud Storage (2026-10-31): deleting a retained object is 403
/// `retentionPolicyNotMet`; the message names the object and the retain-until time in US
/// Pacific time to the nanosecond, and `overrideUnlockedRetention` is honoured on PATCH only.
#[test]
fn deleting_a_retained_object_names_it_and_ignores_the_override() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["held", "locked"]);
    set_retention(&server, "held", "Unlocked");
    set_retention(&server, "locked", "Locked");
    for name in ["held", "locked"] {
        for query in ["", "?overrideUnlockedRetention=true"] {
            let (status, body) =
                server.request("DELETE", &format!("{}{query}", object_path(name)), "");
            assert_eq!(status, 403, "{name} {query} {body}");
            assert_eq!(
                body["error"]["errors"][0]["reason"],
                "retentionPolicyNotMet"
            );
            assert_eq!(
                body["error"]["message"],
                format!(
                    "Object 'rt-bucket/{name}' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until 2099-01-01T19:04:05.123456789-08:00"
                )
            );
        }
    }
    server.stop();
}

/// `retentionExpireTime` is a gRPC field name, not a JSON one (observed 2026-10-31).
#[test]
fn fields_selection_rejects_the_grpc_retention_expire_time_name() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["fields"]);
    let (status, body) = server.request(
        "GET",
        &format!("{}?fields=name,retentionExpireTime", object_path("fields")),
        "",
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["errors"][0]["reason"], "invalidParameter");
    assert_eq!(
        body["error"]["message"],
        "Invalid field selection retentionExpireTime"
    );
    assert_eq!(body["error"]["errors"][0]["locationType"], "parameter");
    assert_eq!(body["error"]["errors"][0]["location"], "fields");
    let (status, body) = server.request(
        "GET",
        &format!(
            "{}?fields=name,retentionExpirationTime",
            object_path("fields")
        ),
        "",
    );
    assert_eq!(status, 200, "{body}");
    server.stop();
}
