use crate::common;
use crate::common::process::declare_server;

use common::TestDir;
use common::process::{Reply, http_command};
use serde_json::{Value, json};
use std::time::Duration;

const OBJECT_METHODS: [&str; 5] = ["media", "multipart", "PATCH", "PUT", "DELETE"];
const OLD_BYTES: &[u8] = b"original\0payload\xff";
const NEW_BYTES: &[u8] = HASH_BYTES;
// Independently computed reference hashes for the standard ASCII check vector.
const HASH_BYTES: &[u8] = b"123456789";
const MD5: &str = "JfnnlDI7RTiF9RgfG2JNCw==";
const CRC32C: &str = "4waSgw==";

impl Server {
    fn start(dir: &TestDir, args: &[&str]) -> Self {
        Self::launch(http_command(&dir.0).args(args))
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        common::http::Client {
            address: &self.address,
            host: "localhost",
            headers: "",
            timeout: Duration::from_secs(5),
        }
        .request(method, path, headers, body)
        .into_last()
    }

    fn bucket(&self, name: &str) -> Value {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            "Content-Type: application/json\r\n",
            json!({"name":name, "storageClass":"NEARLINE", "labels":{"keep":"original", "replace":"old"}}).to_string().as_bytes(),
        ).json_ok()
    }

    fn upload(&self, name: &str, headers: &str, body: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=media&name={name}"),
            headers,
            body,
        )
    }

    fn get(&self, path: &str) -> Reply {
        self.request("GET", path, "", b"")
    }

    fn seed_object(&self, name: &str) -> (Value, Vec<u8>) {
        self.upload(name, "Content-Type: text/original\r\n", OLD_BYTES)
            .json_ok();
        self.request(
            "PATCH",
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            "Content-Type: application/json\r\n",
            r#"{"contentEncoding":"identity","contentDisposition":"inline","contentLanguage":"en","cacheControl":"private, no-transform","metadata":{"keep":"original","replace":"old","escaped":"quote\"\n日本語"}}"#.as_bytes(),
        ).json_ok();
        let snapshot = self.snapshot(name);
        assert_eq!(snapshot.1, OLD_BYTES);
        assert_eq!(snapshot.0["metageneration"], "2");
        snapshot
    }

    fn snapshot(&self, name: &str) -> (Value, Vec<u8>) {
        let path = format!("/storage/v1/b/test-bucket/o/{name}");
        let metadata = self.get(&path).json_ok();
        let media = self.get(&format!("{path}?alt=media"));
        media.expect(200);
        (metadata, media.body)
    }

    fn mutate_object(&self, method: &str, name: &str, condition: &str) -> Reply {
        match method {
            "media" => self.upload(
                &format!("{name}&{condition}"),
                "Content-Type: text/replacement\r\n",
                NEW_BYTES,
            ),
            "multipart" => {
                let mut body = b"--boundary\r\nContent-Type: application/json\r\n\r\n{\"contentType\":\"text/replacement\",\"metadata\":{\"replace\":\"new\"}}\r\n--boundary\r\nContent-Type: text/replacement\r\n\r\n".to_vec();
                body.extend_from_slice(NEW_BYTES);
                body.extend_from_slice(b"\r\n--boundary--\r\n");
                self.request(
                    "POST",
                    &format!("/upload/storage/v1/b/test-bucket/o?uploadType=multipart&name={name}&{condition}"),
                    "Content-Type: multipart/related; boundary=boundary\r\n",
                    &body,
                )
            }
            "PATCH" | "PUT" => self.request(
                method,
                &format!("/storage/v1/b/test-bucket/o/{name}?{condition}"),
                "Content-Type: application/json\r\n",
                br#"{"contentType":"text/replacement","metadata":{"replace":"new"}}"#,
            ),
            "DELETE" => self.request(
                method,
                &format!("/storage/v1/b/test-bucket/o/{name}?{condition}"),
                "",
                b"",
            ),
            _ => unreachable!(),
        }
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

impl Reply {
    /// A missing media object is an HTML document naming the object, not a JSON error.
    fn missing_media(&self, name: &str) {
        self.expect(404);
        assert_eq!(self.headers["content-type"], "text/html; charset=UTF-8");
        assert_eq!(
            self.body,
            format!("No such object: test-bucket/{name}").into_bytes()
        );
    }

    fn json_fields_error(&self, status: u16) -> Value {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        assert_eq!(
            self.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        let value: Value = serde_json::from_slice(&self.body).unwrap();
        let error = &value["error"];
        assert_eq!(error["code"].as_u64(), Some(u64::from(status)));
        assert!(error["message"].as_str().is_some_and(|v| !v.is_empty()));
        let entries = error["errors"].as_array().expect("error entries");
        assert!(!entries.is_empty());
        for entry in entries {
            for field in ["domain", "reason", "message"] {
                assert!(
                    entry[field].as_str().is_some_and(|v| !v.is_empty()),
                    "{field}: {entry}"
                );
            }
        }
        value
    }

    fn condition_failed(&self) {
        let error = self.json_fields_error(412);
        assert_eq!(error["error"]["errors"][0]["reason"], "conditionNotMet");
    }
}

fn object_condition_matrix(condition: &str, field: &str, not_match: bool) {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    server.bucket("test-bucket");
    for method in OBJECT_METHODS {
        for passes in [false, true] {
            let name = format!("{method}-{passes}");
            let before = server.seed_object(&name);
            let current = before.0[field].as_str().unwrap().parse::<i64>().unwrap();
            let value = if passes == not_match {
                current + 1
            } else {
                current
            };
            let reply = server.mutate_object(method, &name, &format!("{condition}={value}"));
            if !passes {
                reply.condition_failed();
                assert_eq!(server.snapshot(&name), before, "{method} {condition}");
            } else if method == "DELETE" {
                reply.expect_with(204, format_args!("{method} {condition}"));
                assert!(reply.body.is_empty());
                server
                    .get(&format!("/storage/v1/b/test-bucket/o/{name}"))
                    .json_fields_error(404);
                server
                    .get(&format!("/storage/v1/b/test-bucket/o/{name}?alt=media"))
                    .missing_media(&name);
            } else {
                let result = common::without_acl(reply.json_ok());
                let after = server.snapshot(&name);
                assert_eq!(after.0, result, "{method} {condition}");
                if matches!(method, "media" | "multipart") {
                    assert_ne!(after.0["generation"], before.0["generation"]);
                    assert_eq!(after.0["metageneration"], "1");
                    assert_eq!(after.1, NEW_BYTES);
                    assert_eq!(after.0["size"], NEW_BYTES.len().to_string());
                    assert_eq!(after.0["md5Hash"], MD5);
                    assert_eq!(after.0["crc32c"], CRC32C);
                    if method == "multipart" {
                        assert_eq!(after.0["metadata"], json!({"replace":"new"}));
                    } else {
                        assert!(after.0.get("metadata").is_none());
                    }
                } else {
                    for field in ["generation", "size", "md5Hash", "crc32c", "timeCreated"] {
                        assert_eq!(after.0[field], before.0[field], "{method} {field}");
                    }
                    assert_eq!(
                        after.0["metageneration"],
                        (before.0["metageneration"]
                            .as_str()
                            .unwrap()
                            .parse::<i64>()
                            .unwrap()
                            + 1)
                        .to_string()
                    );
                    assert_eq!(after.0["metadata"]["replace"], "new");
                    if method == "PATCH" {
                        let mut expected = before.0["metadata"].clone();
                        expected["replace"] = json!("new");
                        assert_eq!(after.0["metadata"], expected);
                    } else {
                        assert_eq!(after.0["metadata"], json!({"replace":"new"}));
                    }
                    assert_eq!(after.1, before.1);
                }
                for field in [
                    "contentEncoding",
                    "contentDisposition",
                    "contentLanguage",
                    "cacheControl",
                ] {
                    if method == "PATCH" {
                        assert_eq!(after.0[field], before.0[field], "{field}");
                    } else {
                        assert!(after.0.get(field).is_none(), "{method} {field}");
                    }
                }
                assert_eq!(after.0["contentType"], "text/replacement");
            }
        }
    }
    server.stop();
}

#[test]
fn basic_contract_object_generation_match_matrix_preserves_failed_resources() {
    object_condition_matrix("ifGenerationMatch", "generation", false);
}

#[test]
fn basic_contract_object_metageneration_match_matrix_preserves_failed_resources() {
    object_condition_matrix("ifMetagenerationMatch", "metageneration", false);
}

#[test]
fn basic_contract_local_object_generation_not_match_mutations_use_412_without_side_effects() {
    object_condition_matrix("ifGenerationNotMatch", "generation", true);
}

#[test]
fn basic_contract_local_object_metageneration_not_match_mutations_use_412_without_side_effects() {
    object_condition_matrix("ifMetagenerationNotMatch", "metageneration", true);
}

fn bucket_condition_matrix(not_match: bool) {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    let condition = if not_match {
        "ifMetagenerationNotMatch"
    } else {
        "ifMetagenerationMatch"
    };
    for method in ["PATCH", "PUT", "DELETE"] {
        for passes in [false, true] {
            let name = format!("bucket-{}-{passes}", method.to_ascii_lowercase());
            server.bucket(&name);
            let path = format!("/storage/v1/b/{name}");
            let before = server.get(&path).json_ok();
            let current = before["metageneration"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .unwrap();
            let value = if passes == not_match {
                current + 1
            } else {
                current
            };
            let body = if method == "DELETE" {
                b"".as_slice()
            } else {
                br#"{"storageClass":"COLDLINE","labels":{"replace":"new"}}"#.as_slice()
            };
            let reply = server.request(
                method,
                &format!("{path}?{condition}={value}"),
                "Content-Type: application/json\r\n",
                body,
            );
            if !passes {
                if not_match && method == "PATCH" {
                    // Observed on Cloud Storage: an empty 304 even with writable settings.
                    reply.expect(304);
                    assert!(reply.body.is_empty());
                } else {
                    reply.condition_failed();
                }
                assert_eq!(server.get(&path).json_ok(), before, "{method} {condition}");
            } else if method == "DELETE" {
                reply.expect(204);
                assert!(reply.body.is_empty());
                server.get(&path).json_fields_error(404);
            } else {
                let after = common::without_acl(reply.json_ok());
                assert_eq!(server.get(&path).json_ok(), after);
                assert_eq!(after["metageneration"], (current + 1).to_string());
                assert_eq!(after["storageClass"], "COLDLINE");
                for field in [
                    "kind",
                    "id",
                    "name",
                    "selfLink",
                    "projectNumber",
                    "location",
                    "locationType",
                    "timeCreated",
                ] {
                    assert_eq!(after[field], before[field], "{method} {field}");
                }
                assert_eq!(after["labels"]["replace"], "new");
                if method == "PATCH" {
                    assert_eq!(after["labels"]["keep"], "original");
                } else {
                    assert_eq!(after["labels"], json!({"replace":"new"}));
                }
            }
        }
    }
    server.stop();
}

#[test]
fn basic_contract_bucket_metageneration_match_matrix_preserves_failed_resources() {
    bucket_condition_matrix(false);
}

#[test]
fn basic_contract_local_bucket_metageneration_not_match_mutations_use_412_without_side_effects() {
    bucket_condition_matrix(true);
}

#[test]
fn basic_contract_generation_match_zero_rejects_live_reads_and_metadata_mutations() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    server.bucket("test-bucket");
    let before = server.seed_object("existing");
    for suffix in ["", "&alt=media"] {
        server
            .get(&format!(
                "/storage/v1/b/test-bucket/o/existing?ifGenerationMatch=0{suffix}"
            ))
            .condition_failed();
        assert_eq!(server.snapshot("existing"), before);
    }
    for method in ["PATCH", "PUT", "DELETE"] {
        server
            .mutate_object(method, "existing", "ifGenerationMatch=0")
            .condition_failed();
        assert_eq!(server.snapshot("existing"), before, "{method}");
    }
    server.stop();
}

#[test]
fn basic_contract_multipart_metadata_only_checksums_validate_stored_bytes() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    server.bucket("test-bucket");
    for (field, hash) in [("md5Hash", MD5), ("crc32c", CRC32C)] {
        let metadata = json!({"name": field, field: hash});
        let body = format!(
            "--boundary\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--boundary\r\nContent-Type: application/octet-stream\r\n\r\n123456789\r\n--boundary--\r\n"
        );
        let uploaded = server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=multipart",
                "Content-Type: multipart/related; boundary=boundary\r\n",
                body.as_bytes(),
            )
            .json_ok();
        assert_eq!(uploaded["md5Hash"], MD5);
        assert_eq!(uploaded["crc32c"], CRC32C);
        assert_eq!(uploaded["size"], HASH_BYTES.len().to_string());
        assert_eq!(server.snapshot(field), (uploaded, HASH_BYTES.to_vec()));
    }
    server.stop();
}

#[test]
fn basic_contract_single_request_resumable_checksums_validate_stored_bytes() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    server.bucket("test-bucket");
    for (name, hash) in [
        ("md5", format!("md5={MD5}")),
        ("crc32c", format!("crc32c={CRC32C}")),
    ] {
        let initiation = server.request(
            "POST",
            &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name={name}"),
            "Content-Type: application/json\r\nX-Upload-Content-Type: application/octet-stream\r\n",
            b"{}",
        );
        initiation.expect(200);
        let path = initiation.headers["location"]
            .strip_prefix("http://localhost")
            .unwrap();
        let uploaded = server.request("PUT", path,
            &format!("Content-Type: application/octet-stream\r\nContent-Range: bytes 0-8/9\r\nX-Goog-Hash: {hash}\r\n"), HASH_BYTES,
        ).json_ok();
        assert_eq!(uploaded["md5Hash"], MD5);
        assert_eq!(uploaded["crc32c"], CRC32C);
        assert_eq!(uploaded["size"], HASH_BYTES.len().to_string());
        assert_eq!(server.snapshot(name), (uploaded, HASH_BYTES.to_vec()));
    }
    server.stop();
}

#[test]
fn basic_contract_media_hash_matrix_stores_computed_hashes_and_ignores_declared_ones() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    server.bucket("test-bucket");
    for (index, hash) in [
        format!("md5={MD5}"),
        format!("crc32c={CRC32C}"),
        format!("md5={MD5},crc32c={CRC32C}"),
    ]
    .iter()
    .enumerate()
    {
        let name = format!("valid-{index}");
        let uploaded = server
            .upload(
                &name,
                &format!("Content-Type: application/octet-stream\r\nX-Goog-Hash: {hash}\r\n"),
                HASH_BYTES,
            )
            .json_ok();
        assert_eq!(uploaded["md5Hash"], MD5);
        assert_eq!(uploaded["crc32c"], CRC32C);
        assert_eq!(server.snapshot(&name), (uploaded, HASH_BYTES.to_vec()));
    }
    for (index, hash) in [
        "md5=AAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
        "crc32c=AAAAAA==".to_owned(),
        format!("md5=AAAAAAAAAAAAAAAAAAAAAA==,crc32c={CRC32C}"),
        format!("md5={MD5},crc32c=AAAAAA=="),
        "md5=AAAAAAAAAAAAAAAAAAAAAA==,crc32c=AAAAAA==".to_owned(),
    ]
    .iter()
    .enumerate()
    {
        // Cloud Storage did not examine X-Goog-Hash on a JSON media upload: a recorded wrong
        // digest was stored with 200 and the computed hashes.
        let name = format!("ignored-{index}");
        let headers = format!("Content-Type: application/octet-stream\r\nX-Goog-Hash: {hash}\r\n");
        let uploaded = server.upload(&name, &headers, HASH_BYTES).json_ok();
        assert_eq!(uploaded["md5Hash"], MD5, "{hash}");
        assert_eq!(uploaded["crc32c"], CRC32C, "{hash}");
        assert_eq!(server.snapshot(&name), (uploaded, HASH_BYTES.to_vec()));
    }
    server.stop();
}

#[test]
fn basic_contract_json_errors_keep_complete_envelopes_with_fields_selection() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, &[]);
    let bucket = server.bucket("test-bucket");
    let object = server.seed_object("existing");
    for (method, path, headers, body, status) in [
        (
            "GET",
            "/storage/v1/b/missing-bucket".to_owned(),
            "",
            b"".as_slice(),
            404,
        ),
        (
            "GET",
            "/storage/v1/b/test-bucket/o/missing-object".to_owned(),
            "",
            b"",
            404,
        ),
        (
            "GET",
            "/storage/v1/b/test-bucket/o/%22q%5C%E6%97%A5".to_owned(),
            "",
            b"",
            404,
        ),
        (
            "GET",
            "/storage/v1/b/test-bucket?ifMetagenerationMatch=invalid".to_owned(),
            "",
            b"",
            400,
        ),
        (
            "GET",
            "/storage/v1/b/test-bucket/o/existing?ifGenerationMatch=invalid".to_owned(),
            "",
            b"",
            400,
        ),
        (
            "PATCH",
            format!(
                "/storage/v1/b/test-bucket?ifMetagenerationMatch={}",
                bucket["metageneration"]
                    .as_str()
                    .unwrap()
                    .parse::<i64>()
                    .unwrap()
                    + 1
            ),
            "Content-Type: application/json\r\n",
            br#"{"labels":{"changed":"yes"}}"#,
            412,
        ),
        (
            "PATCH",
            format!(
                "/storage/v1/b/test-bucket/o/existing?ifGenerationMatch={}",
                object.0["generation"]
                    .as_str()
                    .unwrap()
                    .parse::<i64>()
                    .unwrap()
                    + 1
            ),
            "Content-Type: application/json\r\n",
            br#"{"metadata":{"changed":"yes"}}"#,
            412,
        ),
        (
            "PUT",
            "/storage/v1/b/test-bucket/o/existing".to_owned(),
            "Content-Type: application/json\r\n",
            b"{oops",
            400,
        ),
    ] {
        let expected = server
            .request(method, &path, headers, body)
            .json_fields_error(status);
        for selector in ["name", "*"] {
            let separator = if path.contains('?') { '&' } else { '?' };
            let selected = server
                .request(
                    method,
                    &format!("{path}{separator}fields={selector}"),
                    headers,
                    body,
                )
                .json_fields_error(status);
            assert_eq!(selected, expected, "{method} {path} fields={selector}");
        }
    }
    assert_eq!(server.get("/storage/v1/b/test-bucket").json_ok(), bucket);
    assert_eq!(server.snapshot("existing"), object);
    server.stop();
}

#[test]
fn basic_contract_strict_denials_keep_complete_json_envelopes_with_fields_selection() {
    let dir = TestDir::new();
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin-token"],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"denied@example.invalid","tokens":["denied-token"]}
        ]}),
    );
    let mut server = Server::start(
        &dir,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    let admin = "Authorization: Bearer admin-token\r\n";
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            admin,
            br#"{"name":"test-bucket"}"#,
        )
        .json_ok();
    server.upload("existing", admin, OLD_BYTES).json_ok();
    for path in [
        "/storage/v1/b/test-bucket",
        "/storage/v1/b/test-bucket/o/existing",
    ] {
        for (headers, status) in [
            ("", 401),
            ("Authorization: Bearer invalid-token\r\n", 401),
            ("Authorization: Bearer denied-token\r\n", 403),
        ] {
            let expected = server
                .request("GET", path, headers, b"")
                .json_fields_error(status);
            for selector in ["name", "*"] {
                let selected = server
                    .request("GET", &format!("{path}?fields={selector}"), headers, b"")
                    .json_fields_error(status);
                assert_eq!(selected, expected);
            }
        }
    }
    server.stop();
}

declare_server!();
