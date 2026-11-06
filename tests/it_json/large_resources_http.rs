//! Resources above the former 64 KiB body limit, carried through every path that reads them.
//!
//! Lifting a limit in one place only helps if nothing downstream has its own: list pages, batch
//! parts, gRPC messages, the catalog's constraints and a restart all see these resources.

use crate::common;
use crate::common::process::declare_server;

use common::TestDir;
use common::http::{Client, TextReply as Reply};
use common::process::http_command;
use serde_json::{Value, json};
use std::time::Duration;

const LARGE: &str = "large-m-bucket";
const PROJECT: &str = "project-a";
/// Above the 4 MiB that gRPC clients and servers commonly default to.
const GRPC_DEFAULT: usize = 4 * 1024 * 1024;

declare_server!();

impl Server {
    fn start(dir: &TestDir, options: &[&str]) -> Self {
        Self::launch(http_command(&dir.0.join("data")).args(options))
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Reply {
        Client {
            address: &self.address,
            host: "local.invalid",
            headers: "Content-Type: application/json\r\n",
            timeout: Duration::from_secs(60),
        }
        .request_eof(method, path, "", body.as_bytes())
        .into_text()
    }

    fn insert(&self, body: &Value) -> Value {
        self.request(
            "POST",
            &format!("/storage/v1/b?project={PROJECT}"),
            &body.to_string(),
        )
        .json_ok()
    }

    fn get_large(&self) -> Value {
        self.request("GET", &format!("/storage/v1/b/{LARGE}"), "")
            .json_ok()
    }

    /// Every bucket in the project, following page tokens to the end.
    fn list_all(&self) -> Vec<Value> {
        let mut items = Vec::new();
        let mut token = String::new();
        loop {
            let page = self
                .request(
                    "GET",
                    &format!("/storage/v1/b?project={PROJECT}&pageToken={token}"),
                    "",
                )
                .json_ok();
            let found = page["items"].as_array().cloned().unwrap_or_default();
            assert!(!found.is_empty(), "a continuation page must advance");
            items.extend(found);
            let Some(next) = page["nextPageToken"].as_str() else {
                return items;
            };
            token = next.to_owned();
        }
    }

    fn grpc_address(&self) -> String {
        self.ready["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .find(|listener| listener["scheme"] == "h2c")
            .unwrap()["listen"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

/// The documented maximum: 1,000 prefixes of 1,024 bytes, spread over 100 rules.
fn lifecycle_rules(seed: char) -> Value {
    let rules: Vec<_> = (0..100)
        .map(|rule| {
            let prefixes: Vec<_> = (0..10)
                .map(|index| format!("{seed}{rule:03}{index:03}{}", "p".repeat(1017)))
                .collect();
            json!({"action":{"type":"Delete"},"condition":{"matchesPrefix":prefixes}})
        })
        .collect();
    Value::Array(rules)
}

/// CORS has no documented limit; this takes more than the common 4 MiB gRPC message.
fn cors_rules() -> Value {
    let rules: Vec<_> = (0..5000)
        .map(|index| {
            json!({
                "origin":[format!("https://{index:05}{}.invalid", "o".repeat(1000))],
                "method":["GET"],
                "responseHeader":["x-test"],
                "maxAgeSeconds":60
            })
        })
        .collect();
    Value::Array(rules)
}

fn message(field_and_values: &[(u8, &str)]) -> Vec<u8> {
    let mut message = Vec::new();
    for (tag, value) in field_and_values {
        message.push(*tag);
        let length = u8::try_from(value.len()).unwrap();
        assert!(length < 128, "one-byte varint");
        message.push(length);
        message.extend_from_slice(value.as_bytes());
    }
    message
}

/// One unary gRPC call; returns the `grpc-status` and the size of the response message.
fn grpc_call(address: &str, service_method: &str, message: &[u8]) -> (String, usize) {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    let mut framed = vec![0];
    framed.extend_from_slice(&u32::try_from(message.len()).unwrap().to_be_bytes());
    framed.extend_from_slice(message);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let (mut client, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(stream))
                .await
                .unwrap();
        let driver = tokio::spawn(connection);
        let response = client
            .send_request(
                hyper::Request::post(format!("http://{address}/google.storage.{service_method}"))
                    .header("content-type", "application/grpc")
                    .header("te", "trailers")
                    .body(Full::new(Bytes::from(framed)))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body = response.into_body().collect().await.unwrap();
        let status = body.trailers().unwrap()["grpc-status"]
            .to_str()
            .unwrap()
            .to_owned();
        let bytes = body.to_bytes();
        drop(client);
        driver.abort();
        let _ = driver.await;
        (status, bytes.len().saturating_sub(5))
    })
}

/// Reads the large bucket through every path, each of which once had its own fixed limit.
fn assert_readable(server: &Server, lifecycle: &Value, cors: &Value) {
    let bucket = server.get_large();
    assert_eq!(&bucket["lifecycle"]["rule"], lifecycle);
    assert_eq!(&bucket["cors"], cors);

    let items = server.list_all();
    let names: Vec<_> = items
        .iter()
        .map(|item| item["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["large-a-small", LARGE, "large-z-small"]);
    assert_eq!(&items[1]["cors"], cors);
    assert_eq!(&items[1]["lifecycle"]["rule"], lifecycle);

    let envelope = format!(
        "--part\r\nContent-Type: application/http\r\n\r\nGET /storage/v1/b/{LARGE} HTTP/1.1\r\n\r\n\r\n--part--\r\n"
    );
    let batch = Client {
        address: &server.address,
        host: "local.invalid",
        headers: "Content-Type: multipart/mixed; boundary=part\r\n",
        timeout: Duration::from_secs(60),
    }
    .request_eof("POST", "/batch/storage/v1", "", envelope.as_bytes())
    .into_text();
    batch.expect(200);
    assert!(
        batch.body.contains("HTTP/1.1 200 OK"),
        "{:.200}",
        batch.body
    );
    assert!(batch.body.len() > GRPC_DEFAULT, "{}", batch.body.len());

    let address = server.grpc_address();
    let name = format!("projects/_/buckets/{LARGE}");
    let (status, size) = grpc_call(&address, "v2.Storage/GetBucket", &message(&[(0x0a, &name)]));
    assert_eq!(status, "0");
    assert!(size > GRPC_DEFAULT, "{size}");
    // ListBucketsRequest: parent is field 1 and prefix is field 4.
    let parent = format!("projects/{PROJECT}");
    let (status, size) = grpc_call(
        &address,
        "v2.Storage/ListBuckets",
        &message(&[(0x0a, &parent), (0x22, "large-m")]),
    );
    assert_eq!(status, "0");
    assert!(size > GRPC_DEFAULT, "{size}");
    // The legacy service renders neither CORS nor lifecycle rules, so it keeps its 4 MiB limit on
    // responses. Its GetBucketRequest names the bucket alone in field 1.
    let (status, size) = grpc_call(&address, "v1.Storage/GetBucket", &message(&[(0x0a, LARGE)]));
    assert_eq!(status, "0");
    assert!(size < 1024, "{size}");
}

#[test]
fn a_bucket_above_the_former_limits_survives_every_path_and_a_restart() {
    let dir = TestDir::new();
    const OPTIONS: &[&str] = &["--grpc-listen", "127.0.0.1:0"];
    let mut server = Server::start(&dir, OPTIONS);
    server.insert(&json!({"name":"large-a-small"}));
    server.insert(&json!({"name":"large-z-small"}));
    let (lifecycle, cors) = (lifecycle_rules('a'), cors_rules());
    let created = server.insert(&json!({
        "name":LARGE,
        "lifecycle":{"rule":lifecycle},
        "cors":cors,
        "softDeletePolicy":{"retentionDurationSeconds":"604800"}
    }));
    assert_eq!(created["lifecycle"]["rule"], lifecycle);
    assert_readable(&server, &lifecycle, &cors);

    // A small change keeps the rest of the resource, and a large one replaces its field.
    let path = format!("/storage/v1/b/{LARGE}");
    let patched = server
        .request("PATCH", &path, r#"{"labels":{"changed":"yes"}}"#)
        .json_ok();
    assert_eq!(patched["metageneration"], "2");
    assert_eq!(patched["lifecycle"]["rule"], lifecycle);
    let lifecycle = lifecycle_rules('b');
    let patched = server
        .request(
            "PATCH",
            &path,
            &json!({"lifecycle":{"rule":lifecycle}}).to_string(),
        )
        .json_ok();
    assert_eq!(patched["lifecycle"]["rule"], lifecycle);

    server.stop();
    let server = Server::start(&dir, OPTIONS);
    assert_readable(&server, &lifecycle, &cors);
    let patched = server
        .request("PATCH", &path, r#"{"labels":{"changed":"again"}}"#)
        .json_ok();
    assert_eq!(patched["cors"], cors);

    // A soft-deleted version keeps its own copy of the row.
    let generation = patched["generation"].as_str().unwrap().to_owned();
    server.request("DELETE", &path, "").expect(204);
    let retained = server
        .request(
            "GET",
            &format!("{path}?softDeleted=true&generation={generation}"),
            "",
        )
        .json_ok();
    assert_eq!(retained["cors"], cors);
    server
        .request(
            "POST",
            &format!("{path}/restore?generation={generation}"),
            "",
        )
        .json_ok();
    assert_readable(&server, &lifecycle, &cors);
}

/// CORS and lifecycle rules can each reach the body limit in separate writes, and the response
/// limit has to hold both, or the largest bucket could be stored but not read over gRPC.
#[test]
fn the_largest_bucket_resource_fits_one_grpc_response() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--grpc-listen", "127.0.0.1:0"]);
    let size = 16 * 1024 * 1024 - 1024;
    let origin = format!("https://{}.invalid", "o".repeat(size));
    server.insert(&json!({"name":LARGE,"cors":[{"origin":[origin]}]}));
    let rule = json!({
        "action":{"type":"Delete"},
        "condition":{"matchesPrefix":["p".repeat(size)]}
    });
    server
        .request(
            "PATCH",
            &format!("/storage/v1/b/{LARGE}"),
            &json!({"lifecycle":{"rule":[rule]}}).to_string(),
        )
        .json_ok();
    let name = format!("projects/_/buckets/{LARGE}");
    let (status, encoded) = grpc_call(
        &server.grpc_address(),
        "v2.Storage/GetBucket",
        &message(&[(0x0a, &name)]),
    );
    assert_eq!(status, "0");
    assert!(encoded > 2 * size, "{encoded}");
}

#[test]
fn the_memory_catalog_holds_a_bucket_above_the_former_limits() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--backend", "memory"]);
    let lifecycle = lifecycle_rules('a');
    let created = server.insert(&json!({"name":LARGE,"lifecycle":{"rule":lifecycle}}));
    assert_eq!(created["lifecycle"]["rule"], lifecycle);
    assert_eq!(server.get_large()["lifecycle"]["rule"], lifecycle);
    let lifecycle = lifecycle_rules('b');
    server
        .request(
            "PATCH",
            &format!("/storage/v1/b/{LARGE}"),
            &json!({"lifecycle":{"rule":lifecycle}}).to_string(),
        )
        .json_ok();
    assert_eq!(server.list_all()[0]["lifecycle"]["rule"], lifecycle);
}

#[test]
fn the_documented_rule_and_prefix_counts_stay_enforced() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    let path = format!("/storage/v1/b?project={PROJECT}");
    let rule = |prefixes: usize| {
        let prefixes: Vec<_> = (0..prefixes).map(|index| format!("p{index}")).collect();
        json!({"action":{"type":"Delete"},"condition":{"matchesPrefix":prefixes}})
    };
    let too_many_rules = json!({"name":"rules","lifecycle":{"rule":vec![rule(1);101]}});
    server
        .request("POST", &path, &too_many_rules.to_string())
        .json_reason(400, "invalid");
    let too_many_prefixes = json!({"name":"prefixes","lifecycle":{"rule":[rule(1001)]}});
    server
        .request("POST", &path, &too_many_prefixes.to_string())
        .json_reason(400, "invalid");
}

#[test]
fn an_object_patch_above_64_kib_applies_its_documented_metadata_and_acl() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    server.insert(&json!({"name":LARGE}));
    let path = format!("/upload/storage/v1/b/{LARGE}/o?uploadType=media&name=object");
    server.request("POST", &path, "data").json_ok();
    // 8 KiB of custom metadata, written in six-byte escapes, and a full ACL beside the owner.
    let acl: Vec<_> = (0..99)
        .map(|index| {
            json!({"entity":format!("user-{index:03}{}@example.com", "x".repeat(150)),"role":"READER"})
        })
        .collect();
    let escaped = format!("{}u0041", char::from(0x5c)).repeat(8000);
    let body = format!(
        r#"{{"metadata":{{"key":"{escaped}"}},"acl":{}}}"#,
        Value::Array(acl)
    );
    assert!(body.len() > 64 * 1024, "{}", body.len());
    let object = format!("/storage/v1/b/{LARGE}/o/object");
    let patched = server
        .request("PATCH", &format!("{object}?projection=full"), &body)
        .json_ok();
    assert_eq!(patched["metadata"]["key"], "A".repeat(8000));
    assert_eq!(patched["acl"].as_array().unwrap().len(), 100);
}

/// A session row, its stored completion and a notification keep a copy of the metadata in columns
/// with fixed checks. An oversized value must fail as a client error before any of them: a failed
/// constraint would surface as a retryable 503, after the client had sent its data.
#[test]
fn oversized_fixed_metadata_fails_as_a_client_error_on_every_path() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    server.insert(&json!({"name":LARGE}));
    let upload = format!("/upload/storage/v1/b/{LARGE}/o");
    server
        .request(
            "POST",
            &format!("{upload}?uploadType=media&name=object"),
            "data",
        )
        .json_ok();
    let object = format!("/storage/v1/b/{LARGE}/o/object");
    let metadata = |length: usize| json!({"contentDisposition":"x".repeat(length)}).to_string();
    let too_long = metadata(40 * 1024);
    server
        .request("PATCH", &object, &too_long)
        .json_reason(400, "invalid");
    server
        .request(
            "POST",
            &format!("{upload}?uploadType=resumable&name=refused"),
            &too_long,
        )
        .json_reason(400, "invalid");
    let multipart = format!(
        "--part\r\nContent-Type: application/json\r\n\r\n{too_long}\r\n--part\r\n\r\ndata\r\n--part--\r\n"
    );
    Client {
        address: &server.address,
        host: "local.invalid",
        headers: "Content-Type: multipart/related; boundary=part\r\n",
        timeout: Duration::from_secs(60),
    }
    .request_eof(
        "POST",
        &format!("{upload}?uploadType=multipart&name=refused"),
        "",
        multipart.as_bytes(),
    )
    .into_text()
    .json_reason(400, "invalid");
    server
        .request("GET", &format!("{object}-refused"), "")
        .json_reason(404, "notFound");

    // The largest accepted value, the longest name and the longest field selection fit the rows
    // that a resumable upload writes on its way to completion.
    let accepted = metadata(40 * 1024 - 2);
    let name = "n".repeat(1024);
    let query = format!("uploadType=resumable&name={name}&fields=");
    // The request head has to fit its read buffer, and the session URI repeats this query beside
    // its upload ID.
    let selection = "name,".repeat((15 * 1024 - query.len()) / 5 - 1) + "name";
    let started = server.request("POST", &format!("{upload}?{query}{selection}"), &accepted);
    started.expect(200);
    let session = session_path(&started.headers);
    let finished = server.request("PUT", &session, "data").json_ok();
    assert_eq!(finished, json!({"name":name}));
    let stored = server
        .request("GET", &format!("/storage/v1/b/{LARGE}/o/{name}"), "")
        .json_ok();
    assert_eq!(
        stored["contentDisposition"].as_str().unwrap().len(),
        40 * 1024 - 2
    );
}

fn session_path(headers: &str) -> String {
    let location = headers
        .lines()
        .find_map(|line| line.strip_prefix("location: "))
        .unwrap();
    let (_, path) = location.split_once("/upload/").unwrap();
    format!("/upload/{path}")
}
