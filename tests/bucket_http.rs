mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Command, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    address: String,
    _stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
            .env_clear()
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(&dir.0)
            .args(["--api-base-url", "https://advertised.invalid/api"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready event after catalog recovery");
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            _stdout: stdout,
        }
    }

    fn raw(&self, request: &str) -> Reply {
        raw(&self.address, request)
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Reply {
        self.raw(&format!("{method} {path} HTTP/1.1\r\nHost: untrusted.invalid\r\nX-Forwarded-Host: evil.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
    }

    fn insert(&self, name: &str, project: &str) -> Reply {
        self.request(
            "POST",
            &format!("/storage/v1/b?project={project}"),
            &json!({"name":name}).to_string(),
        )
    }

    fn stop(&mut self) {
        assert!(
            Command::new("kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
    fn error(&self, status: u16, reason: &str) {
        assert_eq!(self.status, status, "{}", self.body);
        assert!(
            self.headers
                .contains("content-type: application/json; charset=UTF-8")
        );
        let value = self.json();
        assert_eq!(value["error"]["code"], status);
        assert_eq!(value["error"]["errors"][0]["reason"], reason);
        assert_eq!(value["error"]["errors"][0]["domain"], "global");
        assert_eq!(
            value["error"]["message"],
            value["error"]["errors"][0]["message"]
        );
    }
}

fn raw(address: &str, request: &str) -> Reply {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    let (headers, body) = reply.split_once("\r\n\r\n").unwrap();
    Reply {
        status: headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
        headers: headers.into(),
        body: body.into(),
    }
}

#[test]
fn bkt_http_crud_metadata_conditions_and_errors() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    let create = server.request("POST", "/storage/v1/b?project=00042", r#"{"name":"test-bucket","location":"EU","storageClass":"NEARLINE","labels":{"team":"dev"}}"#);
    assert_eq!(create.status, 200, "{}", create.body);
    let bucket = create.json();
    assert_eq!(bucket["name"], "test-bucket");
    assert_eq!(bucket["projectNumber"], "42");
    assert_eq!(bucket["metageneration"], "1");
    assert_eq!(bucket["timeCreated"], bucket["updated"]);
    assert_eq!(bucket["location"], "EU");
    assert_eq!(bucket["locationType"], "multi-region");
    assert_eq!(bucket["storageClass"], "NEARLINE");
    assert_eq!(bucket["labels"], json!({"team":"dev"}));
    assert_eq!(
        bucket["selfLink"],
        "https://advertised.invalid/api/storage/v1/b/test-bucket"
    );
    server.insert("test-bucket", "other").error(409, "conflict");
    let path = "/storage/v1/b/test-bucket";
    let get = server.request("GET", path, "");
    assert_eq!(get.status, 200);
    assert_eq!(get.json(), bucket);
    for query in [
        "ifMetagenerationMatch=1",
        "ifMetagenerationNotMatch=2",
        "ifMetagenerationMatch=1&ifMetagenerationNotMatch=2",
        "projection=noAcl&softDeleted=false&prettyPrint=false",
    ] {
        assert_eq!(
            server.request("GET", &format!("{path}?{query}"), "").status,
            200
        );
    }
    for query in [
        "ifMetagenerationMatch=0",
        "ifMetagenerationMatch=-1",
        "ifMetagenerationMatch=2&ifMetagenerationNotMatch=1",
    ] {
        server
            .request("GET", &format!("{path}?{query}"), "")
            .error(412, "conditionNotMet");
    }
    for query in [
        "ifMetagenerationNotMatch=1",
        "ifMetagenerationMatch=1&ifMetagenerationNotMatch=1",
    ] {
        let reply = server.request("GET", &format!("{path}?{query}"), "");
        assert_eq!(reply.status, 304);
        assert!(reply.body.is_empty());
    }
    for query in [
        "ifMetagenerationMatch=2",
        "ifMetagenerationNotMatch=1",
        "ifMetagenerationMatch=2&ifMetagenerationNotMatch=2",
    ] {
        server
            .request("DELETE", &format!("{path}?{query}"), "")
            .error(412, "conditionNotMet");
        assert_eq!(server.request("GET", path, "").json(), bucket);
    }
    let reply = server.request(
        "DELETE",
        &format!("{path}?ifMetagenerationMatch=1&ifMetagenerationNotMatch=2"),
        "",
    );
    assert_eq!(reply.status, 204);
    assert!(reply.body.is_empty());
    for method in ["GET", "DELETE"] {
        server.request(method, path, "").error(404, "notFound");
        server
            .request(method, &format!("{path}?ifMetagenerationMatch=2"), "")
            .error(404, "notFound");
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=00042", "")
            .json(),
        json!({"kind":"storage#buckets"})
    );
}

#[test]
fn bkt_http_query_body_name_and_route_boundaries() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    for method in ["GET", "POST"] {
        server
            .request(method, "/storage/v1/b", "{}")
            .error(400, "required");
    }
    for query in [
        "project=",
        "project=a+b",
        "project=%FF",
        "project=%",
        "project=x&pro%6Aect=x",
        "project=x&unknown=1",
        "project=x&maxResults=-1",
        "project=x&maxResults=4294967296",
        "project=x&softDeleted=wat",
        "project=x&projection=bad",
        "project=x&pageToken=bad",
    ] {
        let reply = server.request("GET", &format!("/storage/v1/b?{query}"), "");
        assert_eq!(reply.status, 400, "{query}: {}", reply.body);
    }
    for query in [
        "projection=full",
        "fields=name",
        "softDeleted=true",
        "returnPartialSuccess=true",
        "userProject=x",
        "key=x",
        "oauth_token=x",
        "prettyPrint=true",
        "quotaUser=x",
        "userIp=x",
        "uploadType=media",
        "callback=x",
        "alt=media",
    ] {
        server
            .request("GET", &format!("/storage/v1/b?project=x&{query}"), "")
            .error(501, "notImplemented");
    }
    for query in [
        "enableObjectRetention=false",
        "predefinedAcl=private",
        "predefinedDefaultObjectAcl=private",
    ] {
        server
            .request(
                "POST",
                &format!("/storage/v1/b?project=x&{query}"),
                r#"{"name":"valid-bucket"}"#,
            )
            .error(501, "notImplemented");
    }
    for (body, status, reason) in [
        ("{", 400, "parseError"),
        ("[]", 400, "invalidArgument"),
        ("{}", 400, "required"),
        (r#"{"name":null}"#, 400, "invalidArgument"),
        (r#"{"name":"ab"}"#, 400, "invalidArgument"),
        (
            r#"{"name":"valid-bucket","labels":{"Bad":"x"}}"#,
            400,
            "invalidArgument",
        ),
        (
            r#"{"name":"valid-bucket","location":"us"}"#,
            501,
            "notImplemented",
        ),
        (
            r#"{"name":"valid-bucket","storageClass":"RAPID"}"#,
            501,
            "notImplemented",
        ),
        (
            r#"{"name":"valid-bucket","versioning":false}"#,
            501,
            "notImplemented",
        ),
        (r#"{"name":"valid-bucket","acl":[]}"#, 501, "notImplemented"),
    ] {
        server
            .request("POST", "/storage/v1/b?project=x", body)
            .error(status, reason);
    }
    for name in [
        "ab",
        "Upper",
        "bad/name",
        "192.168.0.1",
        "goog-test",
        "g00gle-test",
        "bad..name",
        "-bad",
        "bad-",
    ] {
        server.insert(name, "x").error(400, "invalidArgument");
    }
    let max_name = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(30)
    );
    assert_eq!(max_name.len(), 222);
    for name in ["abc".into(), "a".repeat(63), max_name] {
        let created = server.insert(&name, "x");
        assert_eq!(created.status, 200, "{}", created.body);
        assert!(created.json().get("labels").is_none());
    }
    for path in [
        "/storage/v1/b/%",
        "/storage/v1/b/%FF",
        "/storage/v1/b/bad%2Fname",
    ] {
        server
            .request("GET", path, "")
            .error(400, "invalidParameter");
    }
    for (path, allow) in [
        ("/storage/v1/b", "GET, POST"),
        ("/storage/v1/b/abc", "GET, DELETE"),
    ] {
        let reply = server.request("PUT", path, "");
        reply.error(405, "methodNotAllowed");
        assert!(reply.headers.contains(&format!("allow: {allow}")));
    }
    for (method, path) in [
        ("GET", "/storage/v1/b?project=x"),
        ("GET", "/storage/v1/b/abc"),
        ("DELETE", "/storage/v1/b/abc"),
    ] {
        server
            .raw(&format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 999999\r\n\r\n"
            ))
            .error(400, "invalidArgument");
        server
            .raw(&format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n"
            ))
            .error(400, "invalidArgument");
    }
    server.raw("POST /storage/v1/b?project=x HTTP/1.1\r\nHost: localhost\r\nContent-Length: 65537\r\n\r\n").error(413,"uploadTooLarge");
    let exact = format!("{}{}", r#"{"name":"exact-cap"}"#, " ".repeat(65536 - 20));
    assert_eq!(exact.len(), 65536);
    assert_eq!(
        server
            .request("POST", "/storage/v1/b?project=x", &exact)
            .status,
        200
    );
    let chunk = " ".repeat(32768);
    server.raw(&format!("POST /storage/v1/b?project=x HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n8000\r\n{chunk}\r\n8000\r\n{chunk}\r\n1\r\nx\r\n0\r\n\r\n")).error(413,"uploadTooLarge");
    for path in ["/unknown", "/storage/v1/b/", "/storage/v1/b/abc/o"] {
        assert_eq!(
            server
                .raw(&format!(
                    "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 999999\r\n\r\n"
                ))
                .status,
            404
        );
    }
}

#[test]
fn bkt_http_sorted_scoped_pagination_and_limits() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    for (name, project) in [
        ("page-charlie", "p"),
        ("page-alpha", "p"),
        ("page-bravo", "p"),
        ("other-bucket", "p"),
        ("page-elsewhere", "q"),
    ] {
        assert_eq!(server.insert(name, project).status, 200);
    }
    let first = server
        .request(
            "GET",
            "/storage/v1/b?project=p&prefix=page-&maxResults=1",
            "",
        )
        .json();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert_eq!(first["items"][0]["name"], "page-alpha");
    let token = first["nextPageToken"].as_str().unwrap();
    assert_eq!(server.insert("page-aardvark", "p").status, 200);
    let next = server
        .request(
            "GET",
            &format!("/storage/v1/b?project=p&prefix=page-&maxResults=2&pageToken={token}"),
            "",
        )
        .json();
    assert_eq!(
        next["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["page-bravo", "page-charlie"]
    );
    assert!(next.get("nextPageToken").is_none());
    for path in [
        format!("/storage/v1/b?project=q&prefix=page-&pageToken={token}"),
        format!("/storage/v1/b?project=p&pageToken={token}"),
    ] {
        server
            .request("GET", &path, "")
            .error(400, "invalidParameter");
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=p&maxResults=0", "")
            .json(),
        json!({"kind":"storage#buckets"})
    );
    for n in ["1000", "1001", "4294967295"] {
        let reply = server.request(
            "GET",
            &format!("/storage/v1/b?project=p&maxResults={n}"),
            "",
        );
        assert_eq!(reply.status, 200);
        assert_eq!(reply.json()["items"].as_array().unwrap().len(), 5);
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=p&prefix=page-%25", "")
            .json(),
        json!({"kind":"storage#buckets"})
    );
}

#[test]
fn bkt_http_concurrent_duplicates_restart_and_exclusive_directory() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let statuses = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..16)
            .map(|_| scope.spawn(|| server.insert("race-bucket", "project").status))
            .collect();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(statuses.iter().filter(|&&v| v == 200).count(), 1);
    assert_eq!(statuses.iter().filter(|&&v| v == 409).count(), 15);
    let expected = server
        .request("GET", "/storage/v1/b/race-bucket", "")
        .json();
    let second = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .env_clear()
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(&dir.0)
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(second.stdout.is_empty());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already in use"));
    server.stop();
    let mut server = Server::start(&dir);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/race-bucket", "")
            .json(),
        expected
    );
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/race-bucket", "")
            .status,
        204
    );
    server.stop();
    let server = Server::start(&dir);
    server
        .request("GET", "/storage/v1/b/race-bucket", "")
        .error(404, "notFound");
}

#[test]
fn bkt_http_failed_recovery_never_reports_ready() {
    for corrupt in [false, true] {
        let dir = TestDir::new();
        if corrupt {
            std::fs::write(dir.0.join("catalog.sqlite3"), b"not sqlite").unwrap();
        } else {
            std::fs::write(dir.0.join("not-a-directory"), b"file").unwrap();
        }
        let path = if corrupt {
            dir.0.clone()
        } else {
            dir.0.join("not-a-directory")
        };
        let output = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
            .env_clear()
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("catalog unavailable"));
    }
}

#[test]
fn bkt_http_list_clamps_to_1000_with_lookahead() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    for n in 0..1001 {
        assert_eq!(
            server.insert(&format!("limit-{n:04}"), "limits").status,
            200
        );
    }
    for query in ["", "&maxResults=1001", "&maxResults=4294967295"] {
        let first = server
            .request("GET", &format!("/storage/v1/b?project=limits{query}"), "")
            .json();
        assert_eq!(first["items"].as_array().unwrap().len(), 1000);
        assert_eq!(first["items"][999]["name"], "limit-0999");
        let next = server
            .request(
                "GET",
                &format!(
                    "/storage/v1/b?project=limits&pageToken={}",
                    first["nextPageToken"].as_str().unwrap()
                ),
                "",
            )
            .json();
        assert_eq!(next["items"].as_array().unwrap().len(), 1);
        assert_eq!(next["items"][0]["name"], "limit-1000");
        assert!(next.get("nextPageToken").is_none());
    }
}
