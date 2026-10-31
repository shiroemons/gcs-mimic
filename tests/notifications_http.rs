mod object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::{Value, json};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const VIEWER: &str = "Authorization: Bearer viewer\r\n";
const CONFIGS: &str = "/storage/v1/b/test-bucket/notificationConfigs";

fn insert(server: &Server, headers: &str, body: Value) -> Value {
    server
        .request("POST", CONFIGS, headers, body.to_string().as_bytes())
        .json()
}

fn ids(server: &Server, headers: &str) -> Vec<String> {
    let list = server.request("GET", CONFIGS, headers, b"").json();
    assert_eq!(list["kind"], "storage#notifications");
    list["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["id"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn metageneration(server: &Server) -> String {
    server
        .request("GET", "/storage/v1/b/test-bucket", ADMIN, b"")
        .json()["metageneration"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn notification_configs_round_trip_persist_and_follow_bucket_lifecycle() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    let empty = server.request("GET", CONFIGS, "", b"").json();
    assert_eq!(empty, json!({"kind": "storage#notifications"}));

    let minimal = insert(
        &server,
        "",
        json!({"topic": "projects/local/topics/uploads"}),
    );
    assert_eq!(
        minimal,
        json!({
            "kind": "storage#notification",
            "id": "1",
            "selfLink": "http://127.0.0.1:4443/storage/v1/b/test-bucket/notificationConfigs/1",
            "topic": "//pubsub.googleapis.com/projects/local/topics/uploads",
            "payload_format": "JSON_API_V1",
            "etag": "1"
        })
    );
    let full = insert(
        &server,
        "",
        json!({
            "topic": "//pubsub.googleapis.com/projects/local/topics/archive",
            "payload_format": "NONE",
            "event_types": ["OBJECT_DELETE", "OBJECT_ARCHIVE"],
            "custom_attributes": {"team": "storage", "env": "dev"},
            "object_name_prefix": "logs/",
            "id": "999",
            "etag": "ignored"
        }),
    );
    assert_eq!(full["id"], "2");
    assert_eq!(full["payload_format"], "NONE");
    assert_eq!(
        full["event_types"],
        json!(["OBJECT_DELETE", "OBJECT_ARCHIVE"])
    );
    assert_eq!(
        full["custom_attributes"],
        json!({"env": "dev", "team": "storage"})
    );
    assert_eq!(full["object_name_prefix"], "logs/");
    // Creating and deleting configurations increments the bucket metageneration.
    assert_eq!(metageneration(&server), "3");

    assert_eq!(
        server
            .request("GET", &format!("{CONFIGS}/2"), "", b"")
            .json(),
        full
    );
    assert_eq!(
        server
            .request("GET", &format!("{CONFIGS}/2?fields=id,topic"), "", b"")
            .json(),
        json!({"id": "2", "topic": "//pubsub.googleapis.com/projects/local/topics/archive"})
    );
    assert_eq!(
        server
            .request("GET", &format!("{CONFIGS}?fields=items(id)"), "", b"")
            .json(),
        json!({"items": [{"id": "1"}, {"id": "2"}]})
    );

    server.stop();
    let mut server = Server::start(&dir, false);
    assert_eq!(ids(&server, ""), ["1", "2"]);
    assert_eq!(
        server
            .request("GET", &format!("{CONFIGS}/1"), "", b"")
            .json(),
        minimal
    );

    let deleted = server.request("DELETE", &format!("{CONFIGS}/1"), "", b"");
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    for (method, path) in [
        ("GET", format!("{CONFIGS}/1")),
        ("DELETE", format!("{CONFIGS}/1")),
        ("GET", format!("{CONFIGS}/01")),
        ("GET", format!("{CONFIGS}/abc")),
    ] {
        server
            .request(method, &path, "", b"")
            .error(404, "notFound");
    }
    assert_eq!(ids(&server, ""), ["2"]);
    assert_eq!(metageneration(&server), "4");
    // A deleted ID is not reused while the bucket exists.
    assert_eq!(
        insert(
            &server,
            "",
            json!({"topic": "projects/local/topics/uploads"})
        )["id"],
        "3"
    );

    // Deleting the bucket removes its configurations; a recreated bucket starts empty.
    assert_eq!(
        server
            .request("DELETE", "/storage/v1/b/test-bucket", "", b"")
            .status,
        204
    );
    server
        .request("GET", CONFIGS, "", b"")
        .error(404, "notFound");
    server
        .request("GET", &format!("{CONFIGS}/2"), "", b"")
        .error(404, "notFound");
    server
        .request(
            "POST",
            CONFIGS,
            "",
            br#"{"topic":"projects/local/topics/uploads"}"#,
        )
        .error(404, "notFound");
    server.bucket();
    assert!(ids(&server, "").is_empty());
    assert_eq!(
        insert(
            &server,
            "",
            json!({"topic": "projects/local/topics/uploads"})
        )["id"],
        "1"
    );
    server.stop();
}

#[test]
fn notification_requests_reject_invalid_input_without_mutation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    for (body, reason) in [
        (&b"{"[..], "parseError"),
        (b"{}", "required"),
        (br#"{"topic":"uploads"}"#, "invalid"),
        (br#"{"topic":"projects/p/topics/goog-topic"}"#, "invalid"),
        (
            br#"{"topic":"projects/p/topics/topic","payload_format":"JSON"}"#,
            "invalid",
        ),
        (
            br#"{"topic":"projects/p/topics/topic","event_types":["OBJECT_FINALIZE","OBJECT_FINALIZE"]}"#,
            "invalid",
        ),
        (
            br#"{"topic":"projects/p/topics/topic","event_types":["OBJECT_CREATE"]}"#,
            "invalid",
        ),
        (
            br#"{"topic":"projects/p/topics/topic","custom_attributes":{"a":"1","b":"2","c":"3","d":"4","e":"5","f":"6","g":"7","h":"8","i":"9","j":"10","k":"11"}}"#,
            "invalid",
        ),
    ] {
        server
            .request("POST", CONFIGS, "", body)
            .error(400, reason);
    }
    for (method, path, body, status, reason) in [
        ("PUT", CONFIGS.to_owned(), &b""[..], 405, "methodNotAllowed"),
        (
            "PATCH",
            format!("{CONFIGS}/1"),
            b"",
            405,
            "methodNotAllowed",
        ),
        (
            "GET",
            format!("{CONFIGS}?userProject=p"),
            b"",
            400,
            "badRequest",
        ),
        (
            "GET",
            format!("{CONFIGS}?bogus=1"),
            b"",
            400,
            "invalidParameter",
        ),
        (
            "GET",
            format!("{CONFIGS}?fields=bogus"),
            b"",
            400,
            "invalidParameter",
        ),
        ("GET", CONFIGS.to_owned(), b"{}", 400, "invalidArgument"),
        (
            "GET",
            "/storage/v1/b/Bad_Bucket/notificationConfigs".to_owned(),
            b"",
            400,
            "invalidParameter",
        ),
        (
            "GET",
            "/storage/v1/b/missing-bucket/notificationConfigs".to_owned(),
            b"",
            404,
            "notFound",
        ),
    ] {
        server
            .request(method, &path, "", body)
            .error(status, reason);
    }
    let reply = server.request("GET", CONFIGS, "If-None-Match: \"1\"\r\n", b"");
    reply.error(400, "badRequest");
    assert!(ids(&server, "").is_empty());
    assert_eq!(metageneration(&server), "1");

    // Ten all-event configurations reach the per-event-type limit for every event type.
    for _ in 0..10 {
        insert(&server, "", json!({"topic": "projects/p/topics/topic"}));
    }
    server
        .request(
            "POST",
            CONFIGS,
            "",
            br#"{"topic":"projects/p/topics/topic","event_types":["OBJECT_ARCHIVE"]}"#,
        )
        .error(400, "invalidArgument");
    assert_eq!(ids(&server, "").len(), 10);
}

#[test]
fn notification_strict_permissions_follow_bucket_get_and_update() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    server.bucket();
    let created = insert(
        &server,
        ADMIN,
        json!({"topic": "projects/local/topics/uploads"}),
    );
    let item = format!("{CONFIGS}/{}", created["id"].as_str().unwrap());
    // objectViewer lacks storage.buckets.get and storage.buckets.update.
    for (method, path, body) in [
        ("GET", CONFIGS.to_owned(), &b""[..]),
        ("GET", item.clone(), b""),
        (
            "POST",
            CONFIGS.to_owned(),
            br#"{"topic":"projects/local/topics/t2"}"#,
        ),
        ("DELETE", item.clone(), b""),
    ] {
        server
            .request(method, &path, VIEWER, body)
            .error(403, "forbidden");
        server
            .request(method, &path, "", body)
            .error(401, "required");
        server
            .request(method, &path, "Authorization: Bearer unknown\r\n", body)
            .error(401, "authError");
    }
    // A bucket-scoped legacyBucketReader grant allows reads but not configuration changes.
    let mut policy = server
        .request("GET", "/storage/v1/b/test-bucket/iam", ADMIN, b"")
        .json();
    policy["bindings"].as_array_mut().unwrap().push(json!({
        "role": "roles/storage.legacyBucketReader",
        "members": ["user:viewer@example.test"]
    }));
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            policy.to_string().as_bytes(),
        )
        .json();
    assert_eq!(ids(&server, VIEWER), ["1"]);
    assert_eq!(server.request("GET", &item, VIEWER, b"").json(), created);
    server
        .request(
            "POST",
            CONFIGS,
            VIEWER,
            br#"{"topic":"projects/local/topics/t2"}"#,
        )
        .error(403, "forbidden");
    server
        .request("DELETE", &item, VIEWER, b"")
        .error(403, "forbidden");
    assert_eq!(server.request("DELETE", &item, ADMIN, b"").status, 204);
    assert!(ids(&server, ADMIN).is_empty());
}

#[test]
fn deprecated_object_change_notification_routes_are_not_recreated() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    let watch =
        json!({"id": "channel", "type": "web_hook", "address": "https://example.invalid/hook"});
    // `o/watch` stays an opaque object name, whose item route has no POST; channels are unknown.
    for (path, status) in [
        ("/storage/v1/b/test-bucket/o/watch", 405),
        ("/storage/v1/channels/stop", 404),
    ] {
        let reply = server.request("POST", path, "", watch.to_string().as_bytes());
        assert_eq!(
            reply.status,
            status,
            "{path}: {}",
            String::from_utf8_lossy(&reply.body)
        );
    }
    assert!(ids(&server, "").is_empty());
    server
        .request("GET", "/storage/v1/b/test-bucket/o/watch", "", b"")
        .error(404, "notFound");
}

fn queued_messages(dir: &TestDir) -> Vec<Value> {
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    connection
        .prepare("SELECT message FROM notification_outbox ORDER BY id LIMIT 32")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|row| serde_json::from_str::<Value>(&row.unwrap()).unwrap()["messages"][0].clone())
        .collect()
}

#[test]
fn notification_effects_follow_http_mutations_and_persist_without_a_publisher() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    server.bucket();
    insert(
        &server,
        "",
        json!({"topic":"projects/local/topics/uploads", "object_name_prefix":"incoming/"}),
    );
    server.put("unmatched", b"ignored").json();
    let first = server.put("incoming%2Fitem", b"first").json();
    let object = "/storage/v1/b/test-bucket/o/incoming%2Fitem";
    let changed = server
        .request(
            "PATCH",
            object,
            "",
            br#"{"contentType":"application/test","metadata":{"team":"storage"}}"#,
        )
        .json();
    assert_eq!(changed["metageneration"], "2");
    server
        .request(
            "PATCH",
            &format!("{object}?ifMetagenerationMatch=1"),
            "",
            br#"{"contentType":"rejected"}"#,
        )
        .error(412, "conditionNotMet");
    server
        .request("DELETE", &format!("{object}?ifGenerationMatch=0"), "", b"")
        .error(412, "conditionNotMet");
    let second = server.put("incoming%2Fitem", b"second").json();
    assert_eq!(server.request("DELETE", object, "", b"").status, 204);
    server.stop();
    let messages = queued_messages(&dir);
    let events: Vec<_> = messages
        .iter()
        .map(|message| message["attributes"]["eventType"].as_str().unwrap())
        .collect();
    assert_eq!(
        events,
        [
            "OBJECT_FINALIZE",
            "OBJECT_METADATA_UPDATE",
            "OBJECT_DELETE",
            "OBJECT_FINALIZE",
            "OBJECT_DELETE"
        ]
    );
    assert_eq!(messages[0]["attributes"]["objectId"], "incoming/item");
    assert_eq!(
        messages[2]["attributes"]["objectGeneration"],
        first["generation"]
    );
    assert_eq!(
        messages[2]["attributes"]["overwrittenByGeneration"],
        second["generation"]
    );
    let mut restarted = Server::start(&dir, false);
    assert_eq!(queued_messages(&dir), messages);
    restarted.stop();
}

struct EventServer {
    child: std::process::Child,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    address: String,
}

impl EventServer {
    fn start(dir: &TestDir, endpoint: &str, extra: &[&str]) -> Self {
        use std::io::BufRead;
        let mut child = object_effects_support::common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args(["--pubsub-emulator-host", endpoint])
            .args(extra)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            stdout,
            address: ready["listen"].as_str().unwrap().to_owned(),
        }
    }

    fn request(&self, method: &str, path: &str, body: &[u8]) -> object_effects_support::Reply {
        object_effects_support::request(&self.address, method, path, "", body)
    }

    fn stop(&mut self) {
        object_effects_support::common::stop_server(&mut self.child, &mut self.stdout);
    }
}

impl Drop for EventServer {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

fn accept_local_publish(listener: &std::net::TcpListener) -> (String, Value) {
    use std::io::{BufRead, Read, Write};
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
    let mut path = String::new();
    reader.read_line(&mut path).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    assert!((1..=131072).contains(&length));
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\n{\"messageIds\":[\"done\"]}")
        .unwrap();
    (
        path,
        serde_json::from_slice::<Value>(&body).unwrap()["messages"][0].clone(),
    )
}

#[test]
fn legacy_global_notification_rules_and_json_configs_publish_to_the_explicit_local_endpoint() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let (sender, receiver) = std::sync::mpsc::sync_channel(2);
    let publisher = std::thread::spawn(move || {
        for _ in 0..2 {
            sender.send(accept_local_publish(&listener)).unwrap();
        }
    });
    let dir = TestDir::new();
    let mut server = EventServer::start(
        &dir,
        &endpoint,
        &[
            "-event.pubsub-project-id",
            "local",
            "-event.pubsub-topic",
            "global",
            "-event.bucket",
            "test-bucket",
            "-event.object-prefix",
            "incoming/",
            "-event.list",
            "finalize",
        ],
    );
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            br#"{"name":"test-bucket"}"#,
        )
        .json();
    server.request("POST", CONFIGS, br#"{"topic":"projects/local/topics/explicit","payload_format":"NONE","object_name_prefix":"incoming/","custom_attributes":{"team":"storage"}}"#).json();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=unmatched",
            b"ignored",
        )
        .json();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=incoming%2Fitem",
            b"published",
        )
        .json();
    let first = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let second = receiver
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(first.0.contains("/projects/local/topics/global:publish"));
    assert_eq!(first.1["attributes"]["eventType"], "OBJECT_FINALIZE");
    assert!(first.1["attributes"].get("notificationConfig").is_none());
    assert!(first.1.get("data").is_some());
    assert!(second.0.contains("/projects/local/topics/explicit:publish"));
    assert_eq!(second.1["attributes"]["team"], "storage");
    assert_eq!(second.1["attributes"]["payloadFormat"], "NONE");
    assert!(second.1.get("data").is_none());
    publisher.join().unwrap();
    server.stop();
}

#[test]
#[ignore = "requires GCS_MIMIC_TEST_PUBSUB_EMULATOR_HOST pointing to an explicitly started local Pub/Sub emulator"]
fn opted_in_pubsub_emulator_receives_the_notification() {
    let address = std::env::var("GCS_MIMIC_TEST_PUBSUB_EMULATOR_HOST")
        .expect("explicit local Pub/Sub test endpoint is required");
    let socket: std::net::SocketAddr = address
        .parse()
        .expect("use a numeric loopback address:port");
    assert!(socket.ip().is_loopback());
    let dir = TestDir::new();
    let suffix = dir.0.file_name().unwrap().to_str().unwrap();
    let topic = format!("projects/local/topics/{suffix}");
    let subscription = format!("projects/local/subscriptions/{suffix}");
    object_effects_support::request(&address, "PUT", &format!("/v1/{topic}"), "", b"{}").json();
    object_effects_support::request(
        &address,
        "PUT",
        &format!("/v1/{subscription}"),
        "",
        json!({"topic": topic}).to_string().as_bytes(),
    )
    .json();
    let mut server = EventServer::start(&dir, &address, &[]);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            br#"{"name":"test-bucket"}"#,
        )
        .json();
    server
        .request(
            "POST",
            CONFIGS,
            json!({"topic":topic,"payload_format":"NONE"})
                .to_string()
                .as_bytes(),
        )
        .json();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=received",
            b"payload",
        )
        .json();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let result = object_effects_support::request(
            &address,
            "POST",
            &format!("/v1/{subscription}:pull"),
            "",
            br#"{"maxMessages":10,"returnImmediately":true}"#,
        )
        .json();
        if let Some(messages) = result["receivedMessages"].as_array()
            && !messages.is_empty()
        {
            assert_eq!(
                messages[0]["message"]["attributes"]["eventType"],
                "OBJECT_FINALIZE"
            );
            assert_eq!(messages[0]["message"]["attributes"]["objectId"], "received");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Pub/Sub emulator did not receive the notification"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    server.stop();
    assert_eq!(
        object_effects_support::request(
            &address,
            "DELETE",
            &format!("/v1/{subscription}"),
            "",
            b""
        )
        .status,
        200
    );
    assert_eq!(
        object_effects_support::request(&address, "DELETE", &format!("/v1/{topic}"), "", b"")
            .status,
        200
    );
}
