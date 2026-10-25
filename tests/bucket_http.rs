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
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir) -> Self {
        Self::start_options(dir, &[])
    }

    fn start_options(dir: &TestDir, options: &[&str]) -> Self {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args(["--api-base-url", "https://advertised.invalid/api"])
            .args(options)
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
            stdout,
        }
    }

    fn raw(&self, request: &str) -> Reply {
        raw(&self.address, request)
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Reply {
        self.raw(&format!("{method} {path} HTTP/1.1\r\nHost: untrusted.invalid\r\nX-Forwarded-Host: evil.invalid\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
    }

    fn authorized(&self, token: &str, method: &str, path: &str, body: &str) -> Reply {
        self.raw(&format!("{method} {path} HTTP/1.1\r\nHost: local.invalid\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
    }

    fn insert(&self, name: &str, project: &str) -> Reply {
        self.request(
            "POST",
            &format!("/storage/v1/b?project={project}"),
            &json!({"name":name}).to_string(),
        )
    }

    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
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
        assert!(self.headers.contains("content-type: application/json"));
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

const PROFILES: [&str; 2] = ["native", "legacy-emulator"];

#[test]
fn bucket_list_omitted_project_obeys_profile_and_seed_project() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let mut server = Server::start_options(
            &dir,
            &[
                "--compat-profile",
                profile,
                "--seed-project",
                "migration-project",
            ],
        );
        assert_eq!(
            server
                .insert("migration-bucket", "migration-project")
                .status,
            200
        );
        assert_eq!(server.insert("other-bucket", "other-project").status, 200);
        let listed = server.request("GET", "/storage/v1/b", "");
        if profile == "legacy-emulator" {
            assert_eq!(listed.status, 200, "{}", listed.body);
            assert_eq!(listed.json()["items"].as_array().unwrap().len(), 1);
            assert_eq!(listed.json()["items"][0]["name"], "migration-bucket");
        } else {
            listed.error(400, "required");
            assert!(
                listed.json()["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("?project=local-project")
            );
        }
        server
            .request("GET", "/storage/v1/b?project=", "")
            .error(400, "invalidParameter");
        let listed = server.request("GET", "/storage/v1/b?project=other-project", "");
        assert_eq!(listed.status, 200);
        assert_eq!(listed.json()["items"][0]["name"], "other-bucket");
        server.stop();
    }
}

#[test]
fn bucket_put_replaces_omitted_fields_and_preserves_resources() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let mut server = profile_server(&dir, profile);
        let original = server.insert("put-bucket", "00042").json();
        let path = "/storage/v1/b/put-bucket";
        let object = "/storage/v1/b/put-bucket/o/item";
        let upload = "/upload/storage/v1/b/put-bucket/o?uploadType=media&name=item";
        let metadata = server.request("POST", upload, "original bytes").json();
        let policy = server
            .request(
                "PUT",
                &format!("{path}/iam"),
                r#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
            )
            .json();
        let populated = server.request("PUT", path, r#"{"labels":{"keep":"yes","replace":"old"},"storageClass":"ARCHIVE","versioning":{"enabled":true},"defaultEventBasedHold":true}"#);
        assert_eq!(populated.status, 200, "{}", populated.body);
        let merged = server
            .request("PATCH", path, r#"{"labels":{"replace":"new"}}"#)
            .json();
        assert_eq!(merged["labels"], json!({"keep":"yes","replace":"new"}));
        assert_eq!(merged["storageClass"], "ARCHIVE");
        assert_settings(&merged, profile, true, true);
        let replaced = server.request("PUT", path, r#"{"labels":{"replace":"only"}}"#);
        assert_eq!(replaced.status, 200, "{}", replaced.body);
        let value = replaced.json();
        assert_eq!(value["labels"], json!({"replace":"only"}));
        assert_eq!(value["storageClass"], "STANDARD");
        assert_settings(&value, profile, false, false);
        assert_eq!(value["metageneration"], "4");
        assert_ne!(value["etag"], merged["etag"]);
        for field in [
            "name",
            "id",
            "kind",
            "projectNumber",
            "location",
            "locationType",
            "timeCreated",
            "selfLink",
        ] {
            assert_eq!(value[field], original[field], "{profile}: {field}");
        }
        assert_eq!(server.request("GET", object, "").json(), metadata);
        assert_eq!(
            server
                .request("GET", &format!("{object}?alt=media"), "")
                .body,
            "original bytes"
        );
        assert_eq!(
            server.request("GET", &format!("{path}/iam"), "").json(),
            policy
        );
        assert_eq!(server.request("GET", path, "").json(), value);
        assert_eq!(
            server
                .request("GET", "/storage/v1/b?project=00042", "")
                .json()["items"][0],
            value
        );
        assert_eq!(
            server
                .request(
                    "POST",
                    "/upload/storage/v1/b/put-bucket/o?uploadType=media&name=new",
                    "new"
                )
                .json()["storageClass"],
            "STANDARD"
        );
        server.stop();
        let server = profile_server(&dir, profile);
        assert_eq!(server.request("GET", path, "").json(), value);
        assert_eq!(server.request("GET", object, "").json(), metadata);
        assert_eq!(
            server.request("GET", &format!("{path}/iam"), "").json(),
            policy
        );
    }
}

#[test]
fn bucket_put_empty_null_and_invalid_inputs() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("put-empty", "project");
        let path = "/storage/v1/b/put-empty";
        for body in [
            "{}",
            r#"{"labels":{}}"#,
            r#"{"labels":null}"#,
            r#"{"versioning":{}}"#,
        ] {
            let before = server.request("PUT", path, r#"{"labels":{"keep":"yes"},"storageClass":"ARCHIVE","versioning":{"enabled":true},"defaultEventBasedHold":true}"#).json();
            let reply = server.request("PUT", path, body);
            assert_eq!(reply.status, 200, "{}", reply.body);
            let value = reply.json();
            assert!(value.get("labels").is_none());
            assert_eq!(value["storageClass"], "STANDARD");
            assert_settings(&value, profile, false, false);
            assert_eq!(
                value["metageneration"]
                    .as_str()
                    .unwrap()
                    .parse::<i64>()
                    .unwrap(),
                before["metageneration"]
                    .as_str()
                    .unwrap()
                    .parse::<i64>()
                    .unwrap()
                    + 1
            );
        }
        let before = server.request("GET", path, "").json();
        for body in [
            "",
            "{",
            "{}{}",
            "[]",
            "null",
            r#"{"labels":[]}"#,
            r#"{"labels":""}"#,
            r#"{"labels":{"key":1}}"#,
            r#"{"labels":{"key":null}}"#,
            r#"{"storageClass":null}"#,
            r#"{"storageClass":""}"#,
            r#"{"versioning":true}"#,
            r#"{"defaultEventBasedHold":"true"}"#,
        ] {
            let reply = server.request("PUT", path, body);
            assert_eq!(reply.status, 400, "{profile}: {body}: {}", reply.body);
            assert_eq!(server.request("GET", path, "").json(), before);
        }
        let oversized = format!("{{\"padding\":\"{}\"}}", "x".repeat(65536));
        server
            .request("PUT", path, &oversized)
            .error(413, "uploadTooLarge");
        assert_eq!(server.request("GET", path, "").json(), before);
        let no_effect = server.request("PUT", path, "{}").json();
        assert_eq!(
            no_effect["metageneration"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap(),
            before["metageneration"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap()
                + 1
        );
        assert_ne!(no_effect["etag"], before["etag"]);
    }
}

#[test]
fn bucket_put_null_settings_follow_profile_defaults() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("put-null-settings", "project");
        let path = "/storage/v1/b/put-null-settings";
        for body in [
            r#"{"versioning":null}"#,
            r#"{"versioning":{"enabled":null}}"#,
            r#"{"defaultEventBasedHold":null}"#,
        ] {
            let before = server.request("PUT", path, r#"{"labels":{"keep":"yes"},"storageClass":"ARCHIVE","versioning":{"enabled":true},"defaultEventBasedHold":true}"#).json();
            let reply = server.request("PUT", path, body);
            if profile == "native" {
                reply.error(400, "invalidArgument");
                assert_eq!(server.request("GET", path, "").json(), before);
            } else {
                assert_eq!(reply.status, 200, "{}", reply.body);
                assert_settings(&reply.json(), profile, false, false);
                assert!(reply.json().get("labels").is_none());
                assert_eq!(reply.json()["storageClass"], "STANDARD");
            }
        }
    }
}

#[test]
fn bucket_put_label_boundaries_and_classes() {
    for profile in PROFILES {
        let invalid = if profile == "native" {
            "invalidArgument"
        } else {
            "invalid"
        };
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("put-limits", "project");
        let path = "/storage/v1/b/put-limits";
        let labels: serde_json::Map<_, _> =
            (0..64).map(|i| (format!("key{i}"), json!(""))).collect();
        assert_eq!(
            server
                .request("PUT", path, &json!({"labels":labels}).to_string())
                .status,
            200
        );
        let before = server.request("GET", path, "").json();
        let too_many: serde_json::Map<_, _> =
            (0..65).map(|i| (format!("key{i}"), json!("v"))).collect();
        server
            .request(
                "PUT",
                path,
                &json!({"labels":too_many,"storageClass":"ARCHIVE"}).to_string(),
            )
            .error(400, invalid);
        assert_eq!(server.request("GET", path, "").json(), before);
        assert_eq!(
            server
                .request("PUT", path, r#"{"labels":{"extra":"v"}}"#)
                .json()["labels"],
            json!({"extra":"v"})
        );
        for (key, value) in [
            ("".into(), json!("v")),
            ("Bad".into(), json!("v")),
            ("1bad".into(), json!("v")),
            ("a b".into(), json!("v")),
            ("a".repeat(64), json!("v")),
            ("bad".into(), json!("Upper")),
            ("bad".into(), json!("v".repeat(64))),
            ("bad".into(), Value::Null),
        ] {
            let before = server.request("GET", path, "").json();
            server
                .request("PUT", path, &json!({"labels":{key:value}}).to_string())
                .error(400, invalid);
            assert_eq!(server.request("GET", path, "").json(), before);
        }
        assert_eq!(
            server
                .request(
                    "PUT",
                    path,
                    &json!({"labels":{"é".repeat(63):"あ".repeat(63)}}).to_string()
                )
                .status,
            200
        );
        for class in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            let reply = server.request("PUT", path, &json!({"storageClass":class}).to_string());
            assert_eq!(reply.status, 200, "{}", reply.body);
            assert_eq!(reply.json()["storageClass"], class);
        }
        let before = server.request("GET", path, "").json();
        for class in ["REGIONAL", "RAPID"] {
            server
                .request(
                    "PUT",
                    path,
                    &json!({"storageClass":class,"labels":null}).to_string(),
                )
                .error(501, "notImplemented");
        }
        server
            .request("PUT", path, r#"{"storageClass":"TYPO"}"#)
            .error(400, invalid);
        assert_eq!(server.request("GET", path, "").json(), before);
    }
}

#[test]
fn bucket_patch_full_resource_round_trip_preserves_unchanged_fields() {
    bucket_metadata_round_trip("PATCH");
}

#[test]
fn bucket_put_full_resource_round_trip_preserves_unchanged_fields() {
    bucket_metadata_round_trip("PUT");
}

fn bucket_metadata_round_trip(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        assert_eq!(server.insert("patch-round-trip", "project").status, 200);
        let path = "/storage/v1/b/patch-round-trip";
        assert_eq!(server.request(method, path, r#"{"storageClass":"ARCHIVE","versioning":{"enabled":true},"defaultEventBasedHold":true}"#).status, 200);
        for label in ["first", "second"] {
            let before = server.request("GET", path, "").json();
            let mut body = before.clone();
            body["labels"] = json!({"roundtrip":label});
            let reply = server.request(method, path, &body.to_string());
            assert_eq!(reply.status, 200, "{profile}: {}", reply.body);
            let after = reply.json();
            assert_eq!(after["labels"], body["labels"]);
            let revision = before["metageneration"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            assert_eq!(after["metageneration"], (revision + 1).to_string());
            assert_ne!(after["etag"], before["etag"]);
            let mut unchanged_before = before;
            let mut unchanged_after = after.clone();
            for field in ["labels", "metageneration", "updated", "etag"] {
                unchanged_before.as_object_mut().unwrap().remove(field);
                unchanged_after.as_object_mut().unwrap().remove(field);
            }
            assert_eq!(unchanged_after, unchanged_before);
            assert_eq!(server.request("GET", path, "").json(), after);
        }
    }
}

#[test]
fn bucket_patch_location_comparison_is_case_insensitive() {
    bucket_metadata_location("PATCH");
}

#[test]
fn bucket_put_location_comparison_is_case_insensitive() {
    bucket_metadata_location("PUT");
}

fn bucket_metadata_location(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("patch-location", "project");
        let path = "/storage/v1/b/patch-location";
        for location in if profile == "native" {
            ["us", "Us", "US"]
        } else {
            ["us-central1", "Us-Central1", "US-CENTRAL1"]
        } {
            let before = server.request("GET", path, "").json();
            let reply = server.request(method, path, &json!({"location":location}).to_string());
            assert_eq!(reply.status, 200, "{profile}: {}", reply.body);
            assert_eq!(reply.json()["location"], before["location"]);
        }
        let before = server.request("GET", path, "").json();
        server
            .request(method, path, r#"{"location":"EU","labels":{"lost":"no"}}"#)
            .error(501, "notImplemented");
        assert_eq!(server.request("GET", path, "").json(), before);
        if profile == "legacy-emulator" {
            server
                .request(method, path, r#"{"location":"US","labels":{"lost":"no"}}"#)
                .error(501, "notImplemented");
            assert_eq!(server.request("GET", path, "").json(), before);
        }
    }
}

#[test]
fn bucket_get_rejects_patch_only_acl_query_parameters() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let before = server.insert("patch-query", "project").json();
        for query in [
            "predefinedAcl=private",
            "predefinedDefaultObjectAcl=private",
        ] {
            let path = format!("/storage/v1/b/patch-query?{query}");
            server
                .request("GET", &path, "")
                .error(400, "invalidParameter");
            server
                .request("PATCH", &path, "{}")
                .error(501, "notImplemented");
        }
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/patch-query", "")
                .json(),
            before
        );
    }
}

#[test]
fn bucket_patch_rejects_get_only_generation_query_parameter() {
    bucket_metadata_generation_query("PATCH");
}

#[test]
fn bucket_put_rejects_get_only_generation_query_parameter() {
    bucket_metadata_generation_query("PUT");
}

fn bucket_metadata_generation_query(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let before = server.insert("patch-generation-query", "project").json();
        let path = "/storage/v1/b/patch-generation-query";
        let query_path = format!("{path}?generation=1");
        server
            .request("GET", &query_path, "")
            .error(501, "notImplemented");
        server
            .request(method, &query_path, r#"{"labels":{"lost":"no"}}"#)
            .error(400, "invalidParameter");
        if method == "PUT" {
            let query_path = format!("{path}?softDeleted=false");
            assert_eq!(server.request("GET", &query_path, "").json(), before);
            server
                .request(method, &query_path, "{}")
                .error(400, "invalidParameter");
        }
        assert_eq!(server.request("GET", path, "").json(), before);
    }
}

#[test]
fn bucket_patch_null_settings_follow_profile_defaults() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("patch-null-settings", "project");
        let path = "/storage/v1/b/patch-null-settings";
        for body in [
            r#"{"versioning":null}"#,
            r#"{"versioning":{"enabled":null}}"#,
            r#"{"defaultEventBasedHold":null}"#,
        ] {
            let before = server.request("PATCH", path, r#"{"versioning":{"enabled":true},"defaultEventBasedHold":true,"labels":{"keep":"yes"},"storageClass":"ARCHIVE"}"#).json();
            let reply = server.request("PATCH", path, body);
            if profile == "native" {
                reply.error(400, "invalidArgument");
                assert_eq!(server.request("GET", path, "").json(), before);
            } else {
                assert_eq!(reply.status, 200, "{}", reply.body);
                let after = reply.json();
                let clears_hold = body.contains("defaultEventBasedHold");
                assert_settings(&after, profile, clears_hold, !clears_hold);
                for field in ["labels", "storageClass", "timeCreated", "location"] {
                    assert_eq!(after[field], before[field]);
                }
                assert_eq!(server.request("GET", path, "").json(), after);
            }
        }
    }
}

#[test]
fn bucket_patch_accepts_only_empty_lifecycle_rules() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("patch-lifecycle", "project");
        let path = "/storage/v1/b/patch-lifecycle";
        let before = server.request("PATCH", path, r#"{"versioning":{"enabled":true},"defaultEventBasedHold":true,"labels":{"keep":"yes"}}"#).json();
        let reply = server.request("PATCH", path, r#"{"lifecycle":{"rule":[]}}"#);
        assert_eq!(reply.status, 200, "{}", reply.body);
        let after = reply.json();
        assert_eq!(after["metageneration"], "3");
        for field in [
            "versioning",
            "defaultEventBasedHold",
            "labels",
            "timeCreated",
            "storageClass",
        ] {
            assert_eq!(after[field], before[field]);
        }
        for lifecycle in [
            json!({"rule":[],"extra":null}),
            json!({"rule":null}),
            json!({"rule":[{"action":{"type":"Delete"},"condition":{"age":1}}]}),
        ] {
            server
                .request(
                    "PATCH",
                    path,
                    &json!({"lifecycle":lifecycle,"labels":null}).to_string(),
                )
                .error(501, "notImplemented");
            assert_eq!(server.request("GET", path, "").json(), after);
        }
    }
}

#[test]
fn bucket_patch_missing_bucket_validates_body_before_lookup() {
    bucket_metadata_missing_bucket("PATCH");
}

#[test]
fn bucket_put_missing_bucket_validates_body_before_lookup() {
    bucket_metadata_missing_bucket("PUT");
}

fn bucket_metadata_missing_bucket(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let path = "/storage/v1/b/missing-bucket";
        for body in ["", "{", "[]", r#"{"storageClass":null}"#] {
            assert_eq!(
                server.request(method, path, body).status,
                400,
                "{profile}: {body}"
            );
        }
        if method == "PUT" {
            let labels: serde_json::Map<_, _> =
                (0..65).map(|i| (format!("key{i}"), json!("v"))).collect();
            for query in ["", "?ifMetagenerationMatch=0"] {
                server
                    .request(
                        method,
                        &format!("{path}{query}"),
                        &json!({"labels":labels}).to_string(),
                    )
                    .error(
                        400,
                        if profile == "native" {
                            "invalidArgument"
                        } else {
                            "invalid"
                        },
                    );
            }
        }
        server.request(method, path, "{}").error(404, "notFound");
        assert!(
            server
                .request("GET", "/storage/v1/b?project=project", "")
                .json()
                .get("items")
                .is_none()
        );
    }
}

#[test]
fn bucket_patch_merges_labels_class_and_preserves_resources() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let original = server
        .request(
            "POST",
            "/storage/v1/b?project=00042",
            r#"{"name":"patch-bucket","labels":{"keep":"yes","replace":"old","remove":"old"}}"#,
        )
        .json();
    let path = "/storage/v1/b/patch-bucket";
    let object = "/storage/v1/b/patch-bucket/o/item";
    let upload = "/upload/storage/v1/b/patch-bucket/o?uploadType=media&name=item";
    let metadata = server.request("POST", upload, "original bytes").json();
    let policy = server.request("GET", &format!("{path}/iam"), "").json();
    std::thread::sleep(Duration::from_millis(3));
    let reply = server.request("PATCH", path, r#"{"labels":{"replace":"new","remove":null,"missing":null,"empty":""},"storageClass":"NEARLINE"}"#);
    assert_eq!(reply.status, 200, "{}", reply.body);
    let updated = reply.json();
    assert_eq!(
        updated["labels"],
        json!({"keep":"yes","replace":"new","empty":""})
    );
    assert_eq!(updated["storageClass"], "NEARLINE");
    assert_eq!(updated["metageneration"], "2");
    assert_ne!(updated["etag"], original["etag"]);
    assert_ne!(updated["updated"], original["updated"]);
    for field in [
        "name",
        "id",
        "kind",
        "projectNumber",
        "location",
        "locationType",
        "timeCreated",
        "selfLink",
    ] {
        assert_eq!(updated[field], original[field], "{field}");
    }
    assert_eq!(server.request("GET", object, "").json(), metadata);
    assert_eq!(
        server
            .request("GET", &format!("{object}?alt=media"), "")
            .body,
        "original bytes"
    );
    assert_eq!(
        server.request("GET", &format!("{path}/iam"), "").json(),
        policy
    );
    assert_eq!(server.request("GET", path, "").json(), updated);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=00042", "")
            .json()["items"][0],
        updated
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/patch-bucket/o?uploadType=media&name=new",
                "new"
            )
            .json()["storageClass"],
        "NEARLINE"
    );
    server.stop();
    let server = Server::start(&dir);
    assert_eq!(server.request("GET", path, "").json(), updated);
    assert_eq!(server.request("GET", object, "").json(), metadata);
    assert_eq!(
        server.request("GET", &format!("{path}/iam"), "").json(),
        policy
    );
}

#[test]
fn bucket_patch_empty_null_and_invalid_inputs() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("patch-empty", "project");
        let path = "/storage/v1/b/patch-empty";
        let first = server.request("PATCH", path, r#"{"labels":{"keep":"yes"},"storageClass":"ARCHIVE","versioning":{"enabled":true},"defaultEventBasedHold":true}"#).json();
        let mut revision = 2;
        for body in [
            "{}",
            r#"{"labels":{}}"#,
            r#"{"versioning":{}}"#,
            r#"{"labels":{"missing":null}}"#,
        ] {
            let reply = server.request("PATCH", path, body);
            assert_eq!(reply.status, 200, "{}", reply.body);
            revision += 1;
            let value = reply.json();
            for field in [
                "labels",
                "storageClass",
                "versioning",
                "defaultEventBasedHold",
            ] {
                assert_eq!(value[field], first[field]);
            }
            assert_eq!(value["metageneration"], revision.to_string());
        }
        let value = server.request("PATCH", path, r#"{"labels":null}"#).json();
        assert!(value.get("labels").is_none());
        for body in [
            "",
            "{",
            "{}{}",
            "[]",
            "null",
            r#"{"labels":[]}"#,
            r#"{"labels":""}"#,
            r#"{"labels":{"key":1}}"#,
            r#"{"storageClass":null}"#,
            r#"{"storageClass":""}"#,
            r#"{"versioning":true}"#,
            r#"{"defaultEventBasedHold":"true"}"#,
        ] {
            let reply = server.request("PATCH", path, body);
            assert_eq!(reply.status, 400, "{profile}: {body}: {}", reply.body);
            assert_eq!(server.request("GET", path, "").json(), value);
        }
        let oversized = format!("{{\"padding\":\"{}\"}}", "x".repeat(65536));
        server
            .request("PATCH", path, &oversized)
            .error(413, "uploadTooLarge");
        assert_eq!(server.request("GET", path, "").json(), value);
    }
}

#[test]
fn bucket_patch_label_boundaries_and_classes() {
    for profile in PROFILES {
        let invalid = if profile == "native" {
            "invalidArgument"
        } else {
            "invalid"
        };
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("patch-limits", "project");
        let path = "/storage/v1/b/patch-limits";
        let labels: serde_json::Map<_, _> =
            (0..64).map(|i| (format!("key{i}"), json!(""))).collect();
        assert_eq!(
            server
                .request("PATCH", path, &json!({"labels":labels}).to_string())
                .status,
            200
        );
        let before = server.request("GET", path, "").json();
        server
            .request(
                "PATCH",
                path,
                r#"{"labels":{"extra":"v"},"storageClass":"ARCHIVE"}"#,
            )
            .error(400, invalid);
        assert_eq!(server.request("GET", path, "").json(), before);
        assert_eq!(
            server
                .request("PATCH", path, r#"{"labels":{"extra":"v","key0":null}}"#)
                .status,
            200
        );
        assert_eq!(
            server.request("GET", path, "").json()["labels"]
                .as_object()
                .unwrap()
                .len(),
            64
        );
        let too_many: serde_json::Map<_, _> =
            (0..65).map(|i| (format!("key{i}"), json!("v"))).collect();
        server
            .request("PATCH", path, &json!({"labels":too_many}).to_string())
            .error(400, invalid);
        for (key, value) in [
            ("".into(), json!("v")),
            ("Bad".into(), json!("v")),
            ("1bad".into(), json!("v")),
            ("a b".into(), json!("v")),
            ("a".repeat(64), json!("v")),
            ("bad".into(), json!("Upper")),
            ("bad".into(), json!("v".repeat(64))),
            ("Bad".into(), Value::Null),
        ] {
            let before = server.request("GET", path, "").json();
            server
                .request("PATCH", path, &json!({"labels":{key:value}}).to_string())
                .error(400, invalid);
            assert_eq!(server.request("GET", path, "").json(), before);
        }
        server.request("PATCH", path, r#"{"labels":null}"#);
        assert_eq!(
            server
                .request(
                    "PATCH",
                    path,
                    &json!({"labels":{"é".repeat(63):"あ".repeat(63)}}).to_string()
                )
                .status,
            200
        );
        for class in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            let reply = server.request("PATCH", path, &json!({"storageClass":class}).to_string());
            assert_eq!(reply.status, 200, "{}", reply.body);
            assert_eq!(reply.json()["storageClass"], class);
        }
        let before = server.request("GET", path, "").json();
        for class in ["REGIONAL", "RAPID"] {
            server
                .request(
                    "PATCH",
                    path,
                    &json!({"storageClass":class,"labels":null}).to_string(),
                )
                .error(501, "notImplemented");
        }
        server
            .request("PATCH", path, r#"{"storageClass":"TYPO"}"#)
            .error(400, invalid);
        assert_eq!(server.request("GET", path, "").json(), before);
    }
}

#[test]
fn bucket_patch_unsupported_and_output_inputs_preserve_state() {
    bucket_metadata_unsupported_inputs("PATCH");
}

#[test]
fn bucket_put_unsupported_and_output_inputs_preserve_state() {
    bucket_metadata_unsupported_inputs("PUT");
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("put-lifecycle", "project");
        let path = "/storage/v1/b/put-lifecycle";
        let populated = server.request("PUT", path, r#"{"labels":{"before":"yes"},"versioning":{"enabled":true},"defaultEventBasedHold":true}"#).json();
        let policy = server.request("GET", &format!("{path}/iam"), "").json();
        let reply = server.request(
            "PUT",
            path,
            r#"{"acl":[],"lifecycle":{"rule":[]},"labels":{"after":"yes"}}"#,
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        let after = reply.json();
        assert_eq!(after["labels"], json!({"after":"yes"}));
        assert_settings(&after, profile, false, false);
        assert_ne!(after["etag"], populated["etag"]);
        assert_eq!(
            server.request("GET", &format!("{path}/iam"), "").json(),
            policy
        );
        let reply = server.request("PUT", path, r#"{"versioning":{"enabled":true,"future":1}}"#);
        assert_eq!(reply.status, 200, "{}", reply.body);
        let after = reply.json();
        assert_settings(&after, profile, true, false);
        assert_eq!(server.request("GET", path, "").json(), after);
        for body in [
            json!({"acl":[{"entity":"allUsers","role":"READER"}]}),
            json!({"defaultObjectAcl":[]}),
            json!({"lifecycle":{"rule":[],"extra":null}}),
            json!({"lifecycle":{"rule":null}}),
            json!({"lifecycle":{"rule":[{"action":{"type":"Delete"},"condition":{"age":1}}]}}),
        ] {
            server
                .request("PUT", path, &body.to_string())
                .error(501, "notImplemented");
            assert_eq!(server.request("GET", path, "").json(), after);
            assert_eq!(
                server.request("GET", &format!("{path}/iam"), "").json(),
                policy
            );
        }
    }
}

fn bucket_metadata_unsupported_inputs(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let before = server.insert("patch-input", "project").json();
        let path = "/storage/v1/b/patch-input";
        for field in [
            "acl",
            "autoclass",
            "billing",
            "customPlacementConfig",
            "defaultObjectAcl",
            "encryption",
            "hierarchicalNamespace",
            "ipFilter",
            "lifecycle",
            "logging",
            "objectRetention",
            "retentionPolicy",
            "rpo",
            "satisfiesPZI",
            "satisfiesPZS",
            "softDeletePolicy",
            "website",
        ] {
            for value in [Value::Null, json!({}), json!([]), json!(false)] {
                if method == "PUT" && field == "acl" && value == json!([]) {
                    continue;
                }
                server
                    .request(
                        method,
                        path,
                        &json!({field:value,"labels":{"lost":"no"}}).to_string(),
                    )
                    .error(501, "notImplemented");
                assert_eq!(server.request("GET", path, "").json(), before);
            }
        }
        server
            .request(method, path, r#"{"location":"EU"}"#)
            .error(501, "notImplemented");
        assert_eq!(server.request("GET", path, "").json(), before);
        let fields = [
            "id",
            "name",
            "kind",
            "selfLink",
            "projectNumber",
            "generation",
            "metageneration",
            "etag",
            "timeCreated",
            "updated",
            "locationType",
            "owner",
            "softDeleteTime",
            "hardDeleteTime",
            "futureField",
        ];
        let mut body: serde_json::Map<_, _> = fields
            .into_iter()
            .map(|field| (field.into(), json!("different")))
            .collect();
        body.insert("labels".into(), json!({"works":"yes"}));
        let reply = server.request(method, path, &Value::Object(body).to_string());
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.json()["name"], before["name"]);
        assert_eq!(reply.json()["projectNumber"], before["projectNumber"]);
        assert_eq!(reply.json()["timeCreated"], before["timeCreated"]);
        assert_eq!(reply.json()["labels"], json!({"works":"yes"}));
        assert_eq!(server.request("GET", path, "").json(), reply.json());
    }
}

#[test]
fn bucket_patch_conditions_and_query_contract() {
    bucket_metadata_conditions("PATCH");
}

#[test]
fn bucket_put_conditions_and_query_contract() {
    bucket_metadata_conditions("PUT");
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let before = server.insert("put-precedence", "project").json();
        let path = "/storage/v1/b/put-precedence";
        let conditioned = format!("{path}?ifMetagenerationMatch=0");
        server
            .request("PUT", &conditioned, r#"{"labels":{"Bad":"value"}}"#)
            .error(
                400,
                if profile == "native" {
                    "invalidArgument"
                } else {
                    "invalid"
                },
            );
        server
            .request("PUT", &conditioned, r#"{"retentionPolicy":{}}"#)
            .error(501, "notImplemented");
        assert_eq!(server.request("GET", path, "").json(), before);
        let registry = dir.0.join("identities.json");
        std::fs::write(
            &registry,
            json!({"identities":[
                {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{}}
            ]})
            .to_string(),
        )
        .unwrap();
        drop(server);
        let server = Server::start_options(
            &dir,
            &[
                "--compat-profile",
                profile,
                "--auth-mode",
                "strict",
                "--identity-file",
                registry.to_str().unwrap(),
            ],
        );
        server
            .authorized(
                "viewer",
                "PUT",
                &format!("{path}?ifMetagenerationMatch=bad"),
                "{",
            )
            .error(400, "invalidParameter");
        server
            .authorized("viewer", "PUT", path, "{")
            .error(403, "forbidden");
    }
}

fn bucket_metadata_conditions(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let original = server.insert("patch-conditions", "project").json();
        let path = "/storage/v1/b/patch-conditions";
        for query in [
            "ifMetagenerationMatch=0",
            "ifMetagenerationNotMatch=1",
            "ifMetagenerationMatch=2&ifMetagenerationNotMatch=1",
            "ifMetagenerationMatch=-9223372036854775808",
            "ifMetagenerationMatch=9223372036854775807",
        ] {
            server
                .request(method, &format!("{path}?{query}"), "{}")
                .error(412, "conditionNotMet");
            assert_eq!(server.request("GET", path, "").json(), original);
        }
        for query in [
            "ifMetagenerationMatch=",
            "ifMetagenerationMatch=1.0",
            "ifMetagenerationMatch=9223372036854775808",
            "ifMetagenerationNotMatch=-9223372036854775809",
            "ifMetagenerationMatch=%2B1",
            "ifMetagenerationMatch=1&ifMetagenerationMatch=1",
            "projection=bad",
            "unknown=value",
        ] {
            server
                .request(method, &format!("{path}?{query}"), "{}")
                .error(400, "invalidParameter");
        }
        for query in [
            "predefinedAcl=private",
            "predefinedDefaultObjectAcl=private",
            "userProject=project",
            "predefinedAcl=",
            "fields=",
        ] {
            server
                .request(
                    method,
                    &format!("{path}?{query}"),
                    r#"{"labels":{"bad":"no"}}"#,
                )
                .error(501, "notImplemented");
        }
        assert_eq!(server.request("GET", path, "").json(), original);
        server
            .request(
                method,
                "/storage/v1/b/missing-bucket?ifMetagenerationMatch=0",
                "{}",
            )
            .error(404, "notFound");
        let first = server.request(
            method,
            &format!("{path}?ifMetagenerationMatch=1&ifMetagenerationNotMatch=0&projection=full"),
            "{}",
        );
        assert_eq!(first.status, 200);
        assert_eq!(first.json()["metageneration"], "2");
        assert!(first.json().get("acl").is_none());
        assert_eq!(
            server
                .request(
                    method,
                    &format!("{path}?projection=noAcl&ifMetagenerationNotMatch=1"),
                    "{}"
                )
                .json()["metageneration"],
            "3"
        );
    }
}

#[test]
fn bucket_patch_conditioned_race_has_one_winner() {
    bucket_metadata_race("PATCH");
}

#[test]
fn bucket_put_conditioned_race_has_one_winner() {
    bucket_metadata_race("PUT");
}

fn bucket_metadata_race(method: &'static str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("patch-race", "project");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8).map(|i| {
            let address = server.address.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let body = json!({"labels":{"winner":format!("value{i}")}}).to_string();
                barrier.wait();
                raw(&address, &format!("{method} /storage/v1/b/patch-race?ifMetagenerationMatch=1 HTTP/1.1\r\nHost: local.invalid\r\nContent-Length: {}\r\n\r\n{body}", body.len()))
            })
        }).collect();
        let mut winner = None;
        for handle in handles {
            let reply = handle.join().unwrap();
            if reply.status == 200 {
                assert!(winner.replace(reply.json()).is_none());
            } else {
                reply.error(412, "conditionNotMet");
            }
        }
        let value = winner.unwrap();
        assert_eq!(value["metageneration"], "2");
        assert_eq!(
            server.request("GET", "/storage/v1/b/patch-race", "").json(),
            value
        );
    }
}

#[test]
fn bucket_patch_strict_permissions_and_policy_preservation() {
    bucket_metadata_permissions("PATCH");
}

#[test]
fn bucket_put_strict_permissions_and_policy_preservation() {
    bucket_metadata_permissions("PUT");
}

fn bucket_metadata_permissions(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let registry = dir.0.join("identities.json");
        std::fs::write(&registry, json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},
            {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"project":["roles/storage.bucketViewer"]}},
            {"id":"other@example.test","tokens":["other"],"project_roles":{"elsewhere":["roles/storage.admin"]}},
            {"id":"granted@example.test","tokens":["granted"],"project_roles":{}}
        ]}).to_string()).unwrap();
        let server = Server::start_options(
            &dir,
            &[
                "--compat-profile",
                profile,
                "--auth-mode",
                "strict",
                "--identity-file",
                registry.to_str().unwrap(),
            ],
        );
        let path = "/storage/v1/b/patch-private";
        let before = server
            .authorized(
                "admin",
                "POST",
                "/storage/v1/b?project=project",
                r#"{"name":"patch-private"}"#,
            )
            .json();
        for token in ["viewer", "other", "granted"] {
            for body in [
                "{}",
                "{",
                r#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
            ] {
                server
                    .authorized(token, method, path, body)
                    .error(403, "forbidden");
            }
        }
        server.request(method, path, "{}").error(401, "required");
        server
            .authorized("invalid", method, path, "{")
            .error(401, "authError");
        server
            .authorized(
                "invalid",
                method,
                &format!("{path}?ifMetagenerationMatch=bad"),
                "{}",
            )
            .error(401, "authError");
        server
            .authorized(
                "viewer",
                method,
                &format!("{path}?ifMetagenerationMatch=bad"),
                "{",
            )
            .error(400, "invalidParameter");
        server
            .authorized("viewer", method, "/storage/v1/b/missing-bucket", "{}")
            .error(403, "forbidden");
        server
            .authorized("admin", method, "/storage/v1/b/missing-bucket", "{}")
            .error(404, "notFound");
        server
            .request(method, "/storage/v1/b/missing-bucket", "{}")
            .error(401, "required");
        assert_eq!(server.authorized("admin", "GET", path, "").json(), before);
        let policy_path = format!("{path}/iam");
        let policy = server.authorized("admin", "PUT", &policy_path, r#"{"bindings":[{"role":"roles/storage.admin","members":["user:granted@example.test"]}]}"#).json();
        let reply = server.authorized(
            "granted",
            method,
            &format!("{path}?projection=full"),
            r#"{"labels":{"access":"granted"}}"#,
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(
            server.authorized("admin", "GET", &policy_path, "").json(),
            policy
        );
        assert_eq!(
            server.authorized("admin", "GET", path, "").json(),
            reply.json()
        );
        server.authorized("admin", "PUT", &policy_path, r#"{"bindings":[]}"#);
        server
            .authorized("granted", method, path, "{}")
            .error(403, "forbidden");
        assert_eq!(
            server.authorized("admin", "GET", path, "").json(),
            reply.json()
        );
    }
}

#[test]
fn bucket_patch_settings_control_future_object_effects() {
    bucket_metadata_settings("PATCH");
}

#[test]
fn bucket_put_settings_control_future_object_effects() {
    bucket_metadata_settings("PUT");
}

fn bucket_metadata_settings(method: &str) {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        server.insert("patch-settings", "project");
        let path = "/storage/v1/b/patch-settings";
        let upload = "/upload/storage/v1/b/patch-settings/o?uploadType=media&name=item";
        let object = "/storage/v1/b/patch-settings/o/item";
        let first = server.request("POST", upload, "first").json();
        assert_eq!(
            server
                .request(
                    method,
                    path,
                    r#"{"versioning":{"enabled":true},"defaultEventBasedHold":true}"#
                )
                .status,
            200
        );
        assert_eq!(server.request("GET", object, "").json(), first);
        let second = server.request("POST", upload, "second").json();
        assert_eq!(second["eventBasedHold"], true);
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!(
                        "{object}?generation={}&alt=media",
                        first["generation"].as_str().unwrap()
                    ),
                    ""
                )
                .body,
            "first"
        );
        assert_eq!(
            server
                .request(
                    method,
                    path,
                    if method == "PUT" {
                        r#"{"versioning":{"enabled":true}}"#
                    } else {
                        r#"{"defaultEventBasedHold":false}"#
                    }
                )
                .status,
            200
        );
        server
            .request("POST", upload, "blocked")
            .error(403, "objectUnderActiveHold");
        server
            .request("DELETE", object, "")
            .error(403, "objectUnderActiveHold");
        assert_eq!(
            server
                .request("PATCH", object, r#"{"eventBasedHold":false}"#)
                .status,
            200
        );
        let third = server.request("POST", upload, "third").json();
        assert!(third.get("eventBasedHold").is_none());
        assert_eq!(
            server
                .request(
                    method,
                    path,
                    if method == "PUT" {
                        "{}"
                    } else {
                        r#"{"versioning":{"enabled":false}}"#
                    }
                )
                .status,
            200
        );
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!(
                        "{object}?generation={}&alt=media",
                        second["generation"].as_str().unwrap()
                    ),
                    ""
                )
                .body,
            "second"
        );
        assert_eq!(server.request("POST", upload, "fourth").status, 200);
        server
            .request(
                "GET",
                &format!(
                    "{object}?generation={}",
                    third["generation"].as_str().unwrap()
                ),
                "",
            )
            .error(404, "notFound");
        let versions = server
            .request("GET", "/storage/v1/b/patch-settings/o?versions=true", "")
            .json();
        assert_eq!(versions["items"].as_array().unwrap().len(), 3);
        assert_eq!(
            server
                .request("GET", &format!("{object}?alt=media"), "")
                .body,
            "fourth"
        );
    }
}

#[test]
fn bucket_patch_legacy_rendering_and_restart() {
    bucket_metadata_legacy_restart("PATCH");
}

#[test]
fn bucket_put_legacy_rendering_and_restart() {
    bucket_metadata_legacy_restart("PUT");
}

fn bucket_metadata_legacy_restart(method: &str) {
    let dir = TestDir::new();
    let mut server = profile_server(&dir, "legacy-emulator");
    let original = server.insert("ab", "project").json();
    let path = "/storage/v1/b/ab";
    let reply = server.request(method, path, r#"{"labels":{"team":"storage"},"storageClass":"COLDLINE","versioning":{"enabled":true},"defaultEventBasedHold":true}"#);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(reply.headers.contains("content-type: application/json\r\n"));
    assert!(reply.body.ends_with('\n'));
    let value = reply.json();
    assert_eq!(value["labels"], json!({"team":"storage"}));
    assert_eq!(value["storageClass"], "COLDLINE");
    assert_eq!(value["metageneration"], "2");
    assert_ne!(value["etag"], original["etag"]);
    assert_eq!(value["location"], "US-CENTRAL1");
    assert_eq!(value["locationType"], "region");
    assert_eq!(value["projectNumber"], "0");
    server.stop();
    let mut server = profile_server(&dir, "legacy-emulator");
    assert_eq!(server.request("GET", path, "").json(), value);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=project", "")
            .json()["items"][0],
        value
    );
    server.stop();
    let server = profile_server(&dir, "native");
    server
        .request(method, path, "{}")
        .error(400, "invalidParameter");
    let listed = server
        .request("GET", "/storage/v1/b?project=project", "")
        .json();
    assert_eq!(listed["items"][0]["labels"], value["labels"]);
    assert_eq!(listed["items"][0]["storageClass"], value["storageClass"]);
}

#[test]
fn bucket_profile_paging_preserves_names_and_resource_defaults() {
    let dir = TestDir::new();
    let mut server = profile_server(&dir, "legacy-emulator");
    let names = ["127.0.0.1", "a..b", "ab", "cd", "goog-bucket"];
    for name in names {
        assert_eq!(server.insert(name, "project").status, 200);
    }
    for profile in ["legacy-emulator", "native"] {
        if profile == "native" {
            server.stop();
            server = profile_server(&dir, profile);
        }
        let mut path = "/storage/v1/b?project=project&maxResults=1".to_owned();
        for (index, name) in names.iter().enumerate() {
            let page = server.request("GET", &path, "");
            assert_eq!(page.status, 200, "{}", page.body);
            let value = page.json();
            assert_eq!(value["items"].as_array().unwrap().len(), 1);
            assert_eq!(value["items"][0]["name"], *name);
            if profile == "legacy-emulator" {
                assert_eq!(
                    value["items"][0],
                    server
                        .request("GET", &format!("/storage/v1/b/{name}"), "")
                        .json()
                );
            }
            if index + 1 == names.len() {
                assert!(value.get("nextPageToken").is_none());
            } else {
                let token = value["nextPageToken"].as_str().unwrap();
                path = format!("/storage/v1/b?project=project&maxResults=1&pageToken={token}");
            }
        }
    }
}

#[test]
fn bucket_profile_name_bound_keeps_created_buckets_reachable() {
    let dir = TestDir::new();
    let server = profile_server(&dir, "legacy-emulator");
    let name = "a".repeat(666);
    assert_eq!(server.insert(&name, "project").status, 200);
    assert_eq!(
        server
            .request("GET", &format!("/storage/v1/b/{name}"), "")
            .status,
        200
    );
    let listed = server.request("GET", "/storage/v1/b?project=project&maxResults=1", "");
    assert_eq!(listed.status, 200);
    assert_eq!(listed.json()["items"][0]["name"], name);
    assert_eq!(
        server
            .request("DELETE", &format!("/storage/v1/b/{name}"), "")
            .status,
        200
    );
    for length in [667, 700] {
        let rejected = "b".repeat(length);
        server.insert(&rejected, "project").error(400, "invalid");
        server
            .request("GET", &format!("/storage/v1/b/{rejected}"), "")
            .error(400, "invalidParameter");
    }
    assert!(
        server
            .request("GET", "/storage/v1/b?project=project", "")
            .json()
            .get("items")
            .is_none()
    );
}

#[test]
fn bucket_profile_names_support_object_and_xml_routes() {
    let dir = TestDir::new();
    let names = [
        "ab".into(),
        "goog-name".into(),
        "127.0.0.1".into(),
        "a..b".into(),
        "a".repeat(64),
    ];
    let mut server = profile_server(&dir, "legacy-emulator");
    for name in &names {
        assert_eq!(server.insert(name, "project").status, 200);
        let upload = format!("/upload/storage/v1/b/{name}/o?uploadType=media&name=item");
        assert_eq!(server.request("POST", &upload, "original").status, 200);
        let object = format!("/storage/v1/b/{name}/o/item");
        assert_eq!(server.request("GET", &object, "").json()["name"], "item");
        assert_eq!(
            server
                .request("GET", &format!("{object}?alt=media"), "")
                .body,
            "original"
        );
        assert_eq!(
            server
                .request("GET", &format!("/storage/v1/b/{name}/o"), "")
                .json()["items"][0]["name"],
            "item"
        );
        assert_eq!(
            server.request("GET", &format!("/{name}/item"), "").body,
            "original"
        );
        assert_eq!(
            server.request("HEAD", &format!("/{name}/item"), "").status,
            200
        );
        assert_eq!(server.request("DELETE", &object, "").status, 204);

        let initiated = server.request(
            "POST",
            &format!("/upload/storage/v1/b/{name}/o?uploadType=resumable&name=item"),
            "{}",
        );
        assert_eq!(initiated.status, 200, "{}", initiated.body);
        let location = initiated
            .headers
            .lines()
            .find_map(|line| line.strip_prefix("location: "))
            .unwrap();
        let session = location
            .strip_prefix("https://advertised.invalid/api")
            .unwrap();
        let completed = server.raw(&format!("PUT {session} HTTP/1.1\r\nHost: local.invalid\r\nContent-Length: 7\r\nContent-Range: bytes 0-6/7\r\n\r\nresumed"));
        assert_eq!(completed.status, 200, "{}", completed.body);
        assert_eq!(
            server.request("GET", &format!("/{name}/item"), "").body,
            "resumed"
        );
    }
    server.stop();
    let mut native = profile_server(&dir, "native");
    for name in &names {
        native.insert(name, "project").error(400, "invalidArgument");
        for (method, path) in [
            (
                "POST",
                format!("/upload/storage/v1/b/{name}/o?uploadType=media&name=item"),
            ),
            (
                "PUT",
                format!("/upload/storage/v1/b/{name}/o?upload_id=invalid"),
            ),
            ("GET", format!("/storage/v1/b/{name}/o/item")),
            ("GET", format!("/storage/v1/b/{name}/o")),
            ("DELETE", format!("/storage/v1/b/{name}/o/item")),
        ] {
            native
                .request(method, &path, "")
                .error(400, "invalidParameter");
        }
        assert_eq!(
            native.request("GET", &format!("/{name}/item"), "").status,
            404
        );
    }
    native.stop();
    let server = profile_server(&dir, "legacy-emulator");
    for name in &names {
        let object = format!("/storage/v1/b/{name}/o/item");
        assert_eq!(
            server
                .request("GET", &format!("{object}?alt=media"), "")
                .body,
            "resumed"
        );
        assert_eq!(server.request("DELETE", &object, "").status, 204);
        assert_eq!(
            server
                .request("DELETE", &format!("/storage/v1/b/{name}"), "")
                .status,
            200
        );
    }
}

#[test]
fn bucket_api_settings_drive_versioning_and_hold_effects() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        assert_eq!(server.request("POST", "/storage/v1/b?project=project", r#"{"name":"settings-effects","versioning":{"enabled":true},"defaultEventBasedHold":true}"#).status, 200);
        let upload = "/upload/storage/v1/b/settings-effects/o?uploadType=media&name=item";
        let object = "/storage/v1/b/settings-effects/o/item";
        let first = server.request("POST", upload, "first").json();
        assert_eq!(first["eventBasedHold"], true);
        let generation = first["generation"].as_str().unwrap();
        server
            .request("POST", upload, "blocked")
            .error(403, "objectUnderActiveHold");
        server
            .request("DELETE", object, "")
            .error(403, "objectUnderActiveHold");
        assert_eq!(
            server
                .request("GET", &format!("{object}?alt=media"), "")
                .body,
            "first"
        );
        assert_eq!(
            server
                .request("PATCH", object, r#"{"eventBasedHold":false}"#)
                .status,
            200
        );
        let second = server.request("POST", upload, "second").json();
        assert_eq!(second["eventBasedHold"], true);
        let second_generation = second["generation"].as_str().unwrap();
        assert_ne!(generation, second_generation);
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!("{object}?generation={generation}&alt=media"),
                    ""
                )
                .body,
            "first"
        );
        let listed = server
            .request("GET", "/storage/v1/b/settings-effects/o?versions=true", "")
            .json();
        assert_eq!(listed["items"].as_array().unwrap().len(), 2);
        assert!(listed["items"][0]["timeDeleted"].is_string());
        assert_eq!(
            server
                .request("PATCH", object, r#"{"eventBasedHold":false}"#)
                .status,
            200
        );
        assert_eq!(server.request("DELETE", object, "").status, 204);
        server.request("GET", object, "").error(404, "notFound");
        assert_eq!(
            server
                .request("DELETE", "/storage/v1/b/settings-effects", "")
                .status,
            if profile == "native" { 409 } else { 412 }
        );
        for selected in [generation, second_generation] {
            assert_eq!(
                server
                    .request("DELETE", &format!("{object}?generation={selected}"), "")
                    .status,
                204
            );
        }
        assert_eq!(
            server
                .request("DELETE", "/storage/v1/b/settings-effects", "")
                .status,
            if profile == "native" { 204 } else { 200 }
        );
    }
}

fn profile_server(dir: &TestDir, profile: &str) -> Server {
    Server::start_options(dir, &["--compat-profile", profile])
}

fn assert_settings(value: &Value, profile: &str, versioning: bool, hold: bool) {
    if profile == "native" && !versioning {
        assert!(value.get("versioning").is_none());
    } else {
        assert_eq!(value["versioning"], json!({"enabled":versioning}));
    }
    if profile == "native" && !hold {
        assert!(value.get("defaultEventBasedHold").is_none());
    } else {
        assert_eq!(value["defaultEventBasedHold"], hold);
    }
}

#[test]
fn bucket_core_profile_lookup_names_and_json_framing() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let created = server.request(
            "POST",
            "/storage/v1/b?project=project",
            &json!({"name":"framing-bucket","location": if profile == "native" { "EU" } else { "US-CENTRAL1" }}).to_string(),
        );
        assert_eq!(created.status, 200);
        assert_eq!(
            created.json()["location"],
            if profile == "native" {
                "EU"
            } else {
                "US-CENTRAL1"
            }
        );
        let get = server.request("GET", "/storage/v1/b/framing-bucket", "");
        assert_eq!(get.json(), created.json());
        let invalid = server.request("POST", "/storage/v1/b?project=project", "{");
        let absent = server.request("GET", "/storage/v1/b/absent-bucket", "");
        let deleted = server.request("DELETE", "/storage/v1/b/framing-bucket", "");
        for reply in [created, get, invalid, absent] {
            if profile == "native" {
                assert!(
                    reply
                        .headers
                        .contains("content-type: application/json; charset=UTF-8\r\n")
                );
                assert!(!reply.body.ends_with('\n'));
            } else {
                assert!(reply.headers.contains("content-type: application/json\r\n"));
                assert!(reply.body.ends_with('\n'));
                assert!(!reply.body.ends_with("\n\n"));
            }
        }
        assert_eq!(
            deleted.body,
            if profile == "native" { "" } else { "null\n" }
        );
        for name in ["a", "Uppercase", "-bad", "bad-", "bad!"] {
            for suffix in ["", "/iam"] {
                for method in if suffix.is_empty() {
                    ["GET", "DELETE"]
                } else {
                    ["GET", "PUT"]
                } {
                    let reply = server.request(
                        method,
                        &format!("/storage/v1/b/{name}{suffix}"),
                        if method == "PUT" {
                            r#"{"bindings":[]}"#
                        } else {
                            ""
                        },
                    );
                    if profile == "native" {
                        reply.error(400, "invalidParameter");
                    } else {
                        reply.error(404, "notFound");
                        assert_eq!(reply.json()["error"]["message"], "Not Found");
                    }
                }
            }
        }
        let oversized = "a".repeat(667);
        for suffix in ["", "/iam"] {
            server
                .request("GET", &format!("/storage/v1/b/{oversized}{suffix}"), "")
                .error(400, "invalidParameter");
        }
    }
}

#[test]
fn bucket_core_native_restart_preserves_source_only_names_without_exposing_them() {
    let dir = TestDir::new();
    let names = ["ab", "goog-carryover", "127.0.0.1", "a..b"];
    let mut server = profile_server(&dir, "legacy-emulator");
    for name in names {
        assert_eq!(server.insert(name, "project").status, 200);
    }
    server.stop();
    let mut server = profile_server(&dir, "native");
    for name in names {
        server.insert(name, "project").error(400, "invalidArgument");
        for method in ["GET", "DELETE"] {
            server
                .request(method, &format!("/storage/v1/b/{name}"), "")
                .error(400, "invalidParameter");
        }
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b?project=project", "")
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        names.len()
    );
    server.stop();
    let server = profile_server(&dir, "legacy-emulator");
    for name in names {
        assert_eq!(
            server
                .request("GET", &format!("/storage/v1/b/{name}"), "")
                .status,
            200
        );
        assert_eq!(
            server
                .request("DELETE", &format!("/storage/v1/b/{name}"), "")
                .body,
            "null\n"
        );
    }
}

#[test]
fn legacy_create_location_accepts_only_fixed_region_and_stores_native_us() {
    let dir = TestDir::new();
    let mut server = profile_server(&dir, "legacy-emulator");
    for (index, location) in [
        json!("US-CENTRAL1"),
        json!("us-central1"),
        json!("Us-Central1"),
    ]
    .into_iter()
    .enumerate()
    {
        let body = json!({"name":format!("region-bucket-{index}"),"location":location}).to_string();
        let reply = server.request("POST", "/storage/v1/b?project=project", &body);
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.json()["location"], "US-CENTRAL1");
    }
    for (index, location) in [
        json!("EU"),
        json!("US"),
        json!("ASIA"),
        json!(""),
        Value::Null,
        json!(1),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("invalid-region-{index}");
        let body = json!({"name":name,"location":location}).to_string();
        server
            .request("POST", "/storage/v1/b?project=project", &body)
            .error(501, "notImplemented");
        assert_eq!(
            server
                .request("GET", &format!("/storage/v1/b/{name}"), "")
                .status,
            404
        );
    }
    server.stop();
    let server = Server::start_options(&dir, &["--default-location", "ASIA"]);
    for index in 0..3 {
        assert_eq!(
            server
                .request("GET", &format!("/storage/v1/b/region-bucket-{index}"), "")
                .json()["location"],
            "US"
        );
    }
}

#[test]
fn bucket_core_create_get_defaults() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let create = server.insert("core-bucket", "00042");
        assert_eq!(create.status, 200, "{}", create.body);
        let resource = create.json();
        assert_eq!(resource["kind"], "storage#bucket");
        assert_eq!(resource["name"], "core-bucket");
        assert_eq!(resource["id"], "core-bucket");
        assert_eq!(
            resource["location"],
            if profile == "native" {
                "US"
            } else {
                "US-CENTRAL1"
            }
        );
        assert_eq!(resource["storageClass"], "STANDARD");
        assert_eq!(resource["metageneration"], "1");
        assert_eq!(resource["timeCreated"], resource["updated"]);
        assert_eq!(
            resource["selfLink"],
            "https://advertised.invalid/api/storage/v1/b/core-bucket"
        );
        assert_eq!(
            resource["projectNumber"],
            if profile == "native" { "42" } else { "0" }
        );
        assert_eq!(
            resource["locationType"],
            if profile == "native" {
                "multi-region"
            } else {
                "region"
            }
        );
        if profile == "legacy-emulator" {
            assert_eq!(resource["etag"], "RVRhZw==");
        } else {
            assert!(resource["etag"].as_str().unwrap().starts_with("\"b-"));
        }
        assert_settings(&resource, profile, false, false);
        for query in ["", "?projection=full", "?projection=noAcl"] {
            let get = server.request("GET", &format!("/storage/v1/b/core-bucket{query}"), "");
            assert_eq!(get.status, 200);
            assert_eq!(get.json(), resource);
        }
    }
}

#[test]
fn bucket_core_project_and_names() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = Server::start_options(
            &dir,
            &[
                "--compat-profile",
                profile,
                "--seed-project",
                "fallback-project",
            ],
        );
        let no_project = server.request("POST", "/storage/v1/b", r#"{"name":"no-project"}"#);
        if profile == "native" {
            no_project.error(400, "required");
        } else {
            assert_eq!(no_project.status, 200);
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b?project=fallback-project", "")
                    .json()["items"][0]["name"],
                "no-project"
            );
        }
        for name in [
            "ab".into(),
            "goog-core".into(),
            "google-core".into(),
            "127.0.0.1".into(),
            "a..b".into(),
            "x".repeat(64),
            "a".repeat(223),
        ] {
            let created = server.insert(&name, "project");
            if profile == "native" {
                created.error(400, "invalidArgument");
            } else {
                assert_eq!(created.status, 200, "{name}: {}", created.body);
                assert_eq!(
                    server
                        .request("GET", &format!("/storage/v1/b/{name}"), "")
                        .json(),
                    created.json()
                );
                let deleted = server.request("DELETE", &format!("/storage/v1/b/{name}"), "");
                assert_eq!((deleted.status, deleted.body.as_str()), (200, "null\n"));
            }
        }
        for name in ["a", "UPPER", "-bad", "bad-", "bad space", "a/b", "éé"] {
            assert_eq!(server.insert(name, "project").status, 400, "{name}");
        }
        for query in [
            "project=",
            "project=a&project=b",
            "project=%ff",
            "project=has+space",
        ] {
            assert_eq!(
                server
                    .request(
                        "POST",
                        &format!("/storage/v1/b?{query}"),
                        r#"{"name":"invalid-query"}"#
                    )
                    .status,
                400
            );
        }
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/invalid-query", "")
                .status,
            404
        );
    }
}

#[test]
fn bucket_core_invalid_requests_preserve_state() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let original = server.request("POST", "/storage/v1/b?project=project", r#"{"name":"unchanged-bucket","versioning":{"enabled":true},"defaultEventBasedHold":true}"#).json();
        let duplicate = server.request("POST", "/storage/v1/b?project=other", r#"{"name":"unchanged-bucket","versioning":{"enabled":false},"defaultEventBasedHold":false}"#);
        duplicate.error(409, "conflict");
        if profile == "legacy-emulator" {
            assert!(
                duplicate.json()["error"]["message"]
                    .as_str()
                    .unwrap()
                    .starts_with("A Cloud Storage bucket named 'unchanged-bucket' already exists.")
            );
        }
        for body in [
            "",
            "{",
            "[]",
            "null",
            "true",
            "{}",
            r#"{"name":1}"#,
            r#"{"name":null}"#,
            r#"{"name":"failed-bucket","versioning":true}"#,
            r#"{"name":"failed-bucket","versioning":[]}"#,
            r#"{"name":"failed-bucket","versioning":{"enabled":"true"}}"#,
            r#"{"name":"failed-bucket","versioning":{"enabled":1}}"#,
            r#"{"name":"failed-bucket","defaultEventBasedHold":"false"}"#,
            r#"{"name":"failed-bucket","defaultEventBasedHold":0}"#,
            r#"{"name":"failed-bucket","defaultEventBasedHold":{}}"#,
        ] {
            let reply = server.request("POST", "/storage/v1/b?project=project", body);
            assert_eq!(reply.status, 400, "{profile} {body}: {}", reply.body);
            if profile == "legacy-emulator" {
                assert_eq!(reply.json()["error"]["errors"][0]["reason"], "invalid");
            }
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b/failed-bucket", "")
                    .status,
                404
            );
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b/unchanged-bucket", "")
                    .json(),
                original
            );
        }
        let list = server
            .request("GET", "/storage/v1/b?project=project", "")
            .json();
        assert_eq!(list["items"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn bucket_core_settings_round_trip() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        for (index, (versioning, hold)) in
            [(false, false), (false, true), (true, false), (true, true)]
                .into_iter()
                .enumerate()
        {
            let name = format!("settings-{index}");
            let create = server.request("POST", "/storage/v1/b?project=project", &json!({"name":name,"versioning":{"enabled":versioning},"defaultEventBasedHold":hold}).to_string());
            assert_eq!(create.status, 200, "{}", create.body);
            assert_settings(&create.json(), profile, versioning, hold);
            assert_eq!(
                server
                    .request("GET", &format!("/storage/v1/b/{name}"), "")
                    .json(),
                create.json()
            );
        }
        let empty = server.request(
            "POST",
            "/storage/v1/b?project=project",
            r#"{"name":"empty-versioning","versioning":{"future":123}}"#,
        );
        assert_eq!(empty.status, 200);
        assert_settings(&empty.json(), profile, false, false);
        let sdk = server.request("POST", "/storage/v1/b?project=project", r#"{"name":"sdk-settings","lifecycle":{"rule":[]},"versioning":{"enabled":true},"defaultEventBasedHold":true}"#);
        assert_eq!(sdk.status, 200, "{}", sdk.body);
        assert_settings(&sdk.json(), profile, true, true);
        for lifecycle in [
            json!({"rule":[{"action":{"type":"Delete"},"condition":{"age":1}}]}),
            json!({"rule":[],"future":true}),
        ] {
            server
                .request(
                    "POST",
                    "/storage/v1/b?project=project",
                    &json!({"name":"rejected-lifecycle","lifecycle":lifecycle}).to_string(),
                )
                .error(501, "notImplemented");
        }
    }
}

#[test]
fn bucket_core_unsupported_settings_preserve_state() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        for field in [
            "acl",
            "autoclass",
            "billing",
            "customPlacementConfig",
            "defaultObjectAcl",
            "encryption",
            "hierarchicalNamespace",
            "ipFilter",
            "lifecycle",
            "logging",
            "objectRetention",
            "retentionPolicy",
            "rpo",
            "satisfiesPZI",
            "satisfiesPZS",
            "softDeletePolicy",
            "website",
        ] {
            for value in [
                Value::Null,
                json!({}),
                json!([]),
                json!(false),
                json!({"enabled":true}),
            ] {
                let body = json!({"name":"unsupported-bucket",field:value}).to_string();
                let reply = server.request("POST", "/storage/v1/b?project=project", &body);
                reply.error(501, "notImplemented");
                assert!(
                    reply.json()["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains(field)
                );
                assert_eq!(
                    server
                        .request("GET", "/storage/v1/b/unsupported-bucket", "")
                        .status,
                    404
                );
            }
        }
    }
}

#[test]
fn bucket_core_strict_protection_settings_never_create_an_unprotected_bucket() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = Server::start_options(
            &dir,
            &[
                "--compat-profile",
                profile,
                "--auth-mode",
                "strict",
                "--identity-file",
                "config/identities.json",
            ],
        );
        for settings in [
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
            json!({"retentionPolicy":{"retentionPeriod":"86400"}}),
            json!({"lifecycle":{"rule":[{"action":{"type":"Delete"},"condition":{"age":1}}]}}),
            json!({"acl":[{"entity":"allUsers","role":"READER"}]}),
            json!({"defaultObjectAcl":[{"entity":"allUsers","role":"READER"}]}),
            json!({"encryption":{"defaultKmsKeyName":"unavailable-key"}}),
        ] {
            let mut body = settings;
            body["name"] = "protected-bucket".into();
            server
                .authorized(
                    "local-admin-token",
                    "POST",
                    "/storage/v1/b?project=local-project",
                    &body.to_string(),
                )
                .error(501, "notImplemented");
            server
                .authorized(
                    "local-admin-token",
                    "GET",
                    "/storage/v1/b/protected-bucket",
                    "",
                )
                .error(404, "notFound");
            server.authorized("local-admin-token","PUT","/storage/v1/b/protected-bucket/iam",r#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#).error(404,"notFound");
            server
                .authorized(
                    "local-admin-token",
                    "POST",
                    "/upload/storage/v1/b/protected-bucket/o?uploadType=media&name=object",
                    "private bytes",
                )
                .error(404, "notFound");
            server
                .request(
                    "GET",
                    "/storage/v1/b/protected-bucket/o/object?alt=media",
                    "",
                )
                .error(401, "required");
        }
    }
}

#[test]
fn bucket_core_source_input_shape() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        for (index, extra) in [
            r#""versioning":null"#,
            r#""versioning":{"enabled":null}"#,
            r#""defaultEventBasedHold":null"#,
            r#""location":null,"storageClass":false,"labels":3"#,
        ]
        .into_iter()
        .enumerate()
        {
            let body = format!("{{\"name\":\"source-input-{index}\",{extra}}}");
            let reply = server.request("POST", "/storage/v1/b?project=project", &body);
            if profile == "native" || index == 3 {
                assert!(matches!(reply.status, 400 | 501), "{}", reply.body);
            } else {
                assert_eq!(reply.status, 200, "{}", reply.body);
                assert_settings(&reply.json(), profile, false, false);
                assert_eq!(reply.json()["location"], "US-CENTRAL1");
                assert!(reply.json().get("labels").is_none());
            }
        }
        let trailing = server.request(
            "POST",
            "/storage/v1/b?project=project",
            r#"{"name":"trailing-json"} {"name":"ignored-bucket"}"#,
        );
        assert_eq!(trailing.status, if profile == "native" { 400 } else { 200 });
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/ignored-bucket", "")
                .status,
            404
        );
        let unknown = server.request(
            "POST",
            "/storage/v1/b?project=project",
            r#"{"name":"unknown-field","future":{"enabled":true}}"#,
        );
        assert_eq!(unknown.status, 200);
    }
}

#[test]
fn bucket_core_missing_and_delete_recreate() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let path = "/storage/v1/b/recreate-bucket";
        for method in ["GET", "DELETE"] {
            let reply = server.request(method, path, "");
            reply.error(404, "notFound");
            assert_eq!(
                reply.json()["error"]["message"],
                if profile == "native" {
                    "Bucket not found"
                } else {
                    "Not Found"
                }
            );
        }
        assert_eq!(server.request("POST", "/storage/v1/b?project=project", r#"{"name":"recreate-bucket","versioning":{"enabled":true},"defaultEventBasedHold":true}"#).status, 200);
        let rejected = server.request("DELETE", &format!("{path}?ifMetagenerationMatch=2"), "");
        rejected.error(412, "conditionNotMet");
        assert_eq!(server.request("GET", path, "").status, 200);
        let deleted = server.request("DELETE", path, "");
        assert_eq!(deleted.status, if profile == "native" { 204 } else { 200 });
        assert_eq!(
            deleted.body,
            if profile == "native" { "" } else { "null\n" }
        );
        if profile != "native" {
            assert!(
                deleted
                    .headers
                    .contains("content-type: application/json\r\n")
            );
        }
        server.request("GET", path, "").error(404, "notFound");
        server.request("DELETE", path, "").error(404, "notFound");
        let recreated = server.insert("recreate-bucket", "other-project");
        assert_eq!(recreated.status, 200);
        assert_settings(&recreated.json(), profile, false, false);
        assert_eq!(server.request("GET", path, "").json(), recreated.json());
    }
}

#[test]
fn bucket_core_nonempty_delete_preserves_objects() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let created = server.insert("nonempty-bucket", "project").json();
        let mut objects = Vec::new();
        for (name, data) in [("first", "first bytes"), ("second", "second bytes")] {
            let upload = server.request(
                "POST",
                &format!("/upload/storage/v1/b/nonempty-bucket/o?uploadType=media&name={name}"),
                data,
            );
            assert_eq!(upload.status, 200);
            objects.push((name, data, upload.json()));
        }
        let rejected = server.request("DELETE", "/storage/v1/b/nonempty-bucket", "");
        rejected.error(
            if profile == "native" { 409 } else { 412 },
            if profile == "native" {
                "conflict"
            } else {
                "conditionNotMet"
            },
        );
        if profile != "native" {
            assert_eq!(
                rejected.json()["error"]["message"],
                "bucket must be empty prior to deletion"
            );
        }
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/nonempty-bucket", "")
                .json(),
            created
        );
        for (name, data, metadata) in objects {
            assert_eq!(
                server
                    .request(
                        "GET",
                        &format!("/storage/v1/b/nonempty-bucket/o/{name}"),
                        ""
                    )
                    .json(),
                metadata
            );
            assert_eq!(
                server
                    .request(
                        "GET",
                        &format!("/storage/v1/b/nonempty-bucket/o/{name}?alt=media"),
                        ""
                    )
                    .body,
                data
            );
        }
    }
}

#[test]
fn bucket_core_restart_settings() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let mut server = profile_server(&dir, profile);
        let mut expected = Vec::new();
        for (index, (versioning, hold)) in
            [(false, false), (false, true), (true, false), (true, true)]
                .into_iter()
                .enumerate()
        {
            let name = format!("persist-{index}");
            let created = server.request("POST", "/storage/v1/b?project=project", &json!({"name":name,"versioning":{"enabled":versioning},"defaultEventBasedHold":hold}).to_string());
            assert_eq!(created.status, 200);
            expected.push((name, created.json()));
        }
        server.insert("deleted-bucket", "project");
        server.request("DELETE", "/storage/v1/b/deleted-bucket", "");
        server.stop();
        let mut server = profile_server(&dir, profile);
        for (name, resource) in expected {
            assert_eq!(
                server
                    .request("GET", &format!("/storage/v1/b/{name}"), "")
                    .json(),
                resource
            );
        }
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/deleted-bucket", "")
                .status,
            404
        );
        let reset = server.insert("deleted-bucket", "other").json();
        server.stop();
        let server = profile_server(&dir, profile);
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/deleted-bucket", "")
                .json(),
            reset
        );
        assert_settings(&reset, profile, false, false);
    }
}

#[test]
fn bucket_core_migrates_v7_settings() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let mut server = profile_server(&dir, profile);
        let created = server.insert("migrated-bucket", "project").json();
        server.stop();
        let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        common::downgrade_object_schema(&connection);
        connection.execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; PRAGMA user_version = 7;").unwrap();
        drop(connection);
        let mut server = profile_server(&dir, profile);
        let migrated = server.request("GET", "/storage/v1/b/migrated-bucket", "");
        assert_eq!(migrated.status, 200);
        assert_eq!(migrated.json(), created);
        assert_settings(&migrated.json(), profile, false, false);
        server.stop();
        let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            21
        );
    }
}

#[test]
fn bucket_cors_metadata_round_trip_conditions_replacement_and_restart() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let mut server = profile_server(&dir, profile);
        let path = "/storage/v1/b/cors-bucket";
        let rules = json!([
            {"origin":["https://app.example","https://*.example.com"],"method":["GET","HEAD"],"responseHeader":["ETag","X-Example"],"maxAgeSeconds":0},
            {"origin":["*"],"method":["*","OPTIONS"],"future":true}
        ]);
        let mut normalized = rules.clone();
        normalized[1].as_object_mut().unwrap().remove("future");
        let created = server.request(
            "POST",
            "/storage/v1/b?project=project",
            &json!({"name":"cors-bucket","cors":rules}).to_string(),
        );
        assert_eq!(created.status, 200, "{}", created.body);
        assert_eq!(created.json()["cors"], normalized);
        assert_eq!(server.request("GET", path, "").json(), created.json());
        let labels_only = server
            .request("PATCH", path, r#"{"labels":{"team":"storage"}}"#)
            .json();
        assert_eq!(labels_only["cors"], normalized);
        let updated_rules = json!([{"origin":["https://new.example"],"method":["GET"]}]);
        let updated = server.request(
            "PATCH",
            &format!("{path}?ifMetagenerationMatch=2"),
            &json!({"cors":updated_rules}).to_string(),
        );
        assert_eq!(updated.status, 200, "{}", updated.body);
        assert_eq!(updated.json()["cors"], updated_rules);
        server
            .request(
                "PATCH",
                &format!("{path}?ifMetagenerationMatch=2"),
                r#"{"cors":[]}"#,
            )
            .error(412, "conditionNotMet");
        assert_eq!(server.request("GET", path, "").json(), updated.json());
        server.stop();
        let server = profile_server(&dir, profile);
        assert_eq!(server.request("GET", path, "").json(), updated.json());
        let cleared = server.request("PATCH", path, r#"{"cors":null}"#).json();
        assert!(cleared.get("cors").is_none());
        assert_eq!(
            server
                .request("PATCH", path, &json!({"cors":rules}).to_string())
                .status,
            200
        );
        let replaced = server
            .request("PUT", path, &json!({"cors":updated_rules}).to_string())
            .json();
        assert_eq!(replaced["cors"], updated_rules);
        assert!(
            server
                .request("PUT", path, "{}")
                .json()
                .get("cors")
                .is_none()
        );
        assert_eq!(
            server
                .request("PATCH", path, &json!({"cors":rules}).to_string())
                .status,
            200
        );
        assert!(
            server
                .request("PATCH", path, r#"{"cors":[]}"#)
                .json()
                .get("cors")
                .is_none()
        );
    }
}

#[test]
fn bucket_cors_malformed_input_preserves_entire_resource() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let server = profile_server(&dir, profile);
        let before = server.insert("cors-invalid", "project").json();
        for invalid in [
            json!({}),
            json!(false),
            json!([null]),
            json!([{"origin":"*"}]),
            json!([{"origin":[""]}]),
            json!([{"origin":["https://example.com\r\ninjected"]}]),
            json!([{"method":["GET HEAD"]}]),
            json!([{"responseHeader":["bad header"]}]),
            json!([{"maxAgeSeconds":-1}]),
            json!([{"maxAgeSeconds":2147483648u64}]),
            json!([{"maxAgeSeconds":1.5}]),
            json!([{"maxAgeSeconds":"3600"}]),
        ] {
            let body = json!({"cors":invalid,"labels":{"unchanged":"yes"}}).to_string();
            for method in ["PATCH", "PUT"] {
                let reply = server.request(method, "/storage/v1/b/cors-invalid", &body);
                assert_eq!(
                    reply.status, 400,
                    "{profile} {method} {body}: {}",
                    reply.body
                );
                assert_eq!(
                    server
                        .request("GET", "/storage/v1/b/cors-invalid", "")
                        .json(),
                    before
                );
            }
            let reply = server.request(
                "POST",
                "/storage/v1/b?project=project",
                &json!({"name":"invalid-create","cors":invalid}).to_string(),
            );
            assert_eq!(reply.status, 400, "{profile} {invalid}: {}", reply.body);
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b/invalid-create", "")
                    .status,
                404
            );
        }
    }
}

#[test]
fn bucket_cors_concurrent_conditioned_updates_have_one_winner() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    server.insert("cors-race", "project");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let statuses: Vec<_> = std::thread::scope(|scope| {
        (0..8).map(|index| {
            let address = server.address.clone();
            let barrier = barrier.clone();
            scope.spawn(move || {
                let body = json!({"cors":[{"origin":[format!("https://app-{index}.example")],"method":["GET"]}]}).to_string();
                barrier.wait();
                raw(&address, &format!("PATCH /storage/v1/b/cors-race?ifMetagenerationMatch=1 HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len())).status
            })
        }).collect::<Vec<_>>().into_iter().map(|thread| thread.join().unwrap()).collect()
    });
    assert_eq!(statuses.iter().filter(|&&status| status == 200).count(), 1);
    assert_eq!(statuses.iter().filter(|&&status| status == 412).count(), 7);
    let resource = server.request("GET", "/storage/v1/b/cors-race", "").json();
    assert_eq!(resource["metageneration"], "2");
    assert_eq!(resource["cors"].as_array().unwrap().len(), 1);
}

#[test]
fn default_location_preserves_strict_create_authorization() {
    let dir = TestDir::new();
    let registry = dir.0.join("identities.json");
    std::fs::write(&registry, json!({"identities":[
        {"id":"admin@example.test","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},
        {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"project":["roles/storage.bucketViewer"]}}
    ]}).to_string()).unwrap();
    let server = Server::start_options(
        &dir,
        &[
            "--default-location",
            "EU",
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    let path = "/storage/v1/b?project=project";
    let body = r#"{"name":"default-location-bucket"}"#;
    server.request("POST", path, body).error(401, "required");
    server
        .authorized("viewer", "POST", path, body)
        .error(403, "forbidden");
    let reply = server.authorized("admin", "POST", path, body);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["location"], "EU");
}

#[test]
fn bucket_core_strict_permissions() {
    for profile in PROFILES {
        let dir = TestDir::new();
        let registry = dir.0.join("identities.json");
        std::fs::write(&registry, json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local-project":["roles/storage.admin"]}},
            {"id":"viewer@example.test","tokens":["viewer"],"project_roles":{"local-project":["roles/storage.bucketViewer"]}},
            {"id":"other@example.test","tokens":["other"],"project_roles":{"other-project":["roles/storage.admin"]}}
        ]}).to_string()).unwrap();
        let server = Server::start_options(
            &dir,
            &[
                "--compat-profile",
                profile,
                "--auth-mode",
                "strict",
                "--identity-file",
                registry.to_str().unwrap(),
            ],
        );
        let body = r#"{"name":"strict-settings","versioning":{"enabled":true},"defaultEventBasedHold":true}"#;
        let insert_path = if profile == "native" {
            "/storage/v1/b?project=local-project"
        } else {
            "/storage/v1/b"
        };
        for token in ["viewer", "other"] {
            server
                .authorized(token, "POST", insert_path, body)
                .error(403, "forbidden");
        }
        server
            .authorized(
                "viewer",
                "POST",
                insert_path,
                r#"{"name":"strict-settings","defaultEventBasedHold":"bad"}"#,
            )
            .error(403, "forbidden");
        assert_eq!(
            server
                .authorized(
                    "admin",
                    "POST",
                    insert_path,
                    r#"{"name":"strict-settings","versioning":{"enabled":"bad"}}"#
                )
                .status,
            400
        );
        server
            .request("POST", insert_path, body)
            .error(401, "required");
        server
            .authorized("invalid", "POST", insert_path, body)
            .error(401, "authError");
        assert_eq!(
            server
                .authorized("admin", "GET", "/storage/v1/b/strict-settings", "")
                .status,
            404
        );
        let created = server.authorized("admin", "POST", insert_path, body);
        assert_eq!(created.status, 200, "{}", created.body);
        assert_settings(&created.json(), profile, true, true);
        let path = "/storage/v1/b/strict-settings";
        assert_eq!(
            server.authorized("viewer", "GET", path, "").json(),
            created.json()
        );
        for method in ["GET", "DELETE"] {
            server
                .authorized("other", method, path, "")
                .error(403, "forbidden");
            server.request(method, path, "").error(401, "required");
            server
                .authorized("invalid", method, path, "")
                .error(401, "authError");
        }
        server
            .authorized("viewer", "DELETE", path, "")
            .error(403, "forbidden");
        assert_eq!(
            server.authorized("admin", "GET", path, "").json(),
            created.json()
        );
        assert_eq!(
            server.authorized("admin", "DELETE", path, "").status,
            if profile == "native" { 204 } else { 200 }
        );
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
        "projection=full",
    ] {
        assert_eq!(
            server.request("GET", &format!("{path}?{query}"), "").status,
            200
        );
    }
    assert_eq!(
        server
            .request("GET", &format!("{path}?projection=full"), "")
            .json(),
        bucket
    );
    let full_list = server
        .request("GET", "/storage/v1/b?project=00042&projection=full", "")
        .json();
    assert_eq!(full_list["items"][0], bucket);
    for field in ["acl", "defaultObjectAcl", "owner"] {
        assert!(full_list["items"][0].get(field).is_none());
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
fn bkt_http_malformed_conditions_preserve_all_bucket_methods() {
    let dir = TestDir::new();
    let server = Server::start(&dir);
    let original = server.insert("conditional", "project").json();
    for key in ["ifMetagenerationMatch", "ifMetagenerationNotMatch"] {
        for value in [
            "",
            "abc",
            "%2B1",
            "1.0",
            "9223372036854775808",
            "-9223372036854775809",
            &format!("1&{key}=1"),
        ] {
            for method in ["GET", "PATCH", "PUT", "DELETE"] {
                server
                    .request(
                        method,
                        &format!("/storage/v1/b/conditional?{key}={value}"),
                        if matches!(method, "PATCH" | "PUT") {
                            "{}"
                        } else {
                            ""
                        },
                    )
                    .error(400, "invalidParameter");
            }
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b/conditional", "")
                    .json(),
                original
            );
        }
        // Bucket insertion declares no metageneration condition parameters.
        let reply = server.request(
            "POST",
            &format!("/storage/v1/b?project=project&{key}=1"),
            r#"{"name":"not-created"}"#,
        );
        reply.error(400, "invalidParameter");
        assert_eq!(
            reply.json()["error"]["message"],
            format!("Unknown query parameter for this method: {key}")
        );
        server
            .request("GET", "/storage/v1/b/not-created", "")
            .error(404, "notFound");
    }
}

#[test]
fn bkt_http_conditional_delete_and_metadata_mutation_have_one_winner() {
    for method in ["PATCH", "PUT"] {
        for condition in ["ifMetagenerationMatch=1", "ifMetagenerationNotMatch=2"] {
            let dir = TestDir::new();
            let server = Server::start(&dir);
            for _ in 0..12 {
                server.insert("conditional", "project").json();
                let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
                let address = server.address.clone();
                let waiting = barrier.clone();
                let mutation = std::thread::spawn(move || {
                    waiting.wait();
                    raw(
                        &address,
                        &format!(
                            "{method} /storage/v1/b/conditional?ifMetagenerationMatch=1 HTTP/1.1\r\nHost: local\r\nContent-Length: 2\r\n\r\n{{}}"
                        ),
                    )
                });
                barrier.wait();
                let delete = server.request(
                    "DELETE",
                    &format!("/storage/v1/b/conditional?{condition}"),
                    "",
                );
                let update = mutation.join().unwrap();
                match (delete.status, update.status) {
                    (204, 404) => {
                        update.error(404, "notFound");
                        server
                            .request("GET", "/storage/v1/b/conditional", "")
                            .error(404, "notFound");
                    }
                    (412, 200) => {
                        delete.error(412, "conditionNotMet");
                        assert_eq!(update.json()["metageneration"], "2");
                        assert_eq!(
                            server
                                .request("GET", "/storage/v1/b/conditional", "")
                                .json(),
                            update.json()
                        );
                        assert_eq!(
                            server
                                .request("DELETE", "/storage/v1/b/conditional", "")
                                .status,
                            204
                        );
                    }
                    statuses => panic!("exactly one mutation must succeed: {statuses:?}"),
                }
            }
        }
    }
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
        "fields=",
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
            400,
            "invalidArgument",
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
        ("/storage/v1/b/abc", "GET, DELETE, PATCH, PUT"),
    ] {
        let reply = server.request("OPTIONS", path, "");
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
    for path in ["/INVALID", "/storage/v1/b/", "/storage/v1/b/abc/o"] {
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
    for n in ["0", "1000", "1001", "4294967295"] {
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
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
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
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
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
    for query in [
        "",
        "&maxResults=0",
        "&maxResults=1001",
        "&maxResults=4294967295",
    ] {
        let mut path = format!("/storage/v1/b?project=limits{query}");
        let mut names = Vec::new();
        loop {
            let page = server.request("GET", &path, "").json();
            let items = page["items"].as_array().unwrap();
            assert!(!items.is_empty());
            assert!(items.len() <= 1000);
            names.extend(
                items
                    .iter()
                    .map(|item| item["name"].as_str().unwrap().to_owned()),
            );
            assert!(names.len() <= 1001);
            let Some(token) = page["nextPageToken"].as_str() else {
                break;
            };
            path = format!("/storage/v1/b?project=limits{query}&pageToken={token}");
        }
        assert_eq!(
            names,
            (0..1001)
                .map(|n| format!("limit-{n:04}"))
                .collect::<Vec<_>>()
        );
    }
    let full = server
        .request(
            "GET",
            "/storage/v1/b?project=limits&projection=full&maxResults=1000&pageToken=",
            "",
        )
        .json();
    let count = full["items"].as_array().unwrap().len();
    assert!((1..=200).contains(&count));
    assert_eq!(
        full["items"][count - 1]["name"],
        format!("limit-{:04}", count - 1)
    );
    assert!(full["nextPageToken"].is_string());
    let zero_full = server
        .request(
            "GET",
            "/storage/v1/b?project=limits&projection=full&maxResults=0",
            "",
        )
        .json();
    assert_eq!(zero_full, full);
}

#[test]
fn bucket_cors_large_metadata_lists_page_without_loss() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir);
    let rules = json!(vec![json!({}); 20_000]);
    assert_eq!(rules.to_string().len(), 60_001);
    let expected = (0..60)
        .map(|n| format!("cors-large-{n:02}"))
        .collect::<Vec<_>>();
    for name in &expected {
        let body = json!({"name": name, "cors": rules}).to_string();
        assert!(body.len() < 64 * 1024);
        let reply = server.request("POST", "/storage/v1/b?project=large", &body);
        assert_eq!(reply.status, 200, "{}", reply.body);
    }
    for profile in PROFILES {
        if profile == "legacy-emulator" {
            server.stop();
            server = profile_server(&dir, profile);
        }
        let names_only =
            server.request("GET", "/storage/v1/b?project=large&fields=items(name)", "");
        assert_eq!(names_only.status, 200);
        assert_eq!(names_only.json(), json!({"items":[{"name":expected[0]}]}));
        for fields in ["&fields=items(name),nextPageToken", ""] {
            let mut names = Vec::new();
            let mut path = format!("/storage/v1/b?project=large&maxResults=1000{fields}");
            loop {
                let reply = server.request("GET", &path, "");
                assert_eq!(reply.status, 200, "{}", reply.body);
                let page = reply.json();
                let items = page["items"].as_array().unwrap();
                assert_eq!(
                    items.len(),
                    1,
                    "large records must advance without expanding lookahead"
                );
                if fields.is_empty() {
                    assert_eq!(items[0]["cors"], rules);
                } else {
                    assert_eq!(items[0].as_object().unwrap().len(), 1);
                }
                names.push(items[0]["name"].as_str().unwrap().to_owned());
                assert!(names.len() <= expected.len());
                let Some(token) = page["nextPageToken"].as_str() else {
                    break;
                };
                let limit = if names.len() % 2 == 0 { 1 } else { 7 };
                path = format!(
                    "/storage/v1/b?project=large&maxResults={limit}{fields}&pageToken={token}"
                );
            }
            assert_eq!(names, expected, "{profile} {fields}");
        }
    }
}
