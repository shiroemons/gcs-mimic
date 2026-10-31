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

#[test]
fn locked_retention_read_back_is_accepted_and_a_shorter_time_is_not() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["locked"]);
    let stored = set_retention(&server, "locked", "Locked");
    assert_eq!(
        stored["retention"]["retainUntilTime"],
        "2099-01-02T03:04:05.123Z"
    );

    for method in ["PATCH", "PUT"] {
        let (status, body) = server.request(
            method,
            &object_path("locked"),
            &json!({"retention":stored["retention"]}).to_string(),
        );
        assert_eq!(status, 200, "{method} {body}");
        assert_eq!(body["retention"], stored["retention"]);
    }
    // One millisecond earlier is a real shortening and stays refused.
    let (status, body) = server.request(
        "PATCH",
        &object_path("locked"),
        r#"{"retention":{"mode":"Locked","retainUntilTime":"2099-01-02T03:04:05.122Z"}}"#,
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["errors"][0]["reason"], "invalidArgument");
    server.stop();
}

#[test]
fn unlocked_retention_read_back_needs_no_override_and_a_shorter_time_does() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    setup(&server, &["unlocked"]);
    let stored = set_retention(&server, "unlocked", "Unlocked");

    for method in ["PATCH", "PUT"] {
        let (status, body) = server.request(
            method,
            &object_path("unlocked"),
            &json!({"retention":stored["retention"]}).to_string(),
        );
        assert_eq!(status, 200, "{method} {body}");
    }
    let (status, body) = server.request(
        "PATCH",
        &object_path("unlocked"),
        r#"{"retention":{"mode":"Unlocked","retainUntilTime":"2099-01-02T03:04:05.122Z"}}"#,
    );
    assert_eq!(status, 400, "{body}");
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
