mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

const OBJECT_METHODS: [&str; 5] = ["media", "multipart", "PATCH", "PUT", "DELETE"];
const OLD_BYTES: &[u8] = b"original\0payload\xff";
const NEW_BYTES: &[u8] = HASH_BYTES;
// Independently computed reference hashes for the standard ASCII check vector.
const HASH_BYTES: &[u8] = b"123456789";
const MD5: &str = "JfnnlDI7RTiF9RgfG2JNCw==";
const CRC32C: &str = "4waSgw==";

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir, args: &[&str]) -> Self {
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
            .args(args)
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

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        Reply::read(stream)
    }

    fn bucket(&self, name: &str) -> Value {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            "Content-Type: application/json\r\n",
            json!({"name":name, "storageClass":"NEARLINE", "labels":{"keep":"original", "replace":"old"}}).to_string().as_bytes(),
        ).json()
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
            .json();
        self.request(
            "PATCH",
            &format!("/storage/v1/b/test-bucket/o/{name}"),
            "Content-Type: application/json\r\n",
            r#"{"contentEncoding":"identity","contentDisposition":"inline","contentLanguage":"en","cacheControl":"private, no-transform","metadata":{"keep":"original","replace":"old","escaped":"quote\"\n日本語"}}"#.as_bytes(),
        ).json();
        let snapshot = self.snapshot(name);
        assert_eq!(snapshot.1, OLD_BYTES);
        assert_eq!(snapshot.0["metageneration"], "2");
        snapshot
    }

    fn snapshot(&self, name: &str) -> (Value, Vec<u8>) {
        let path = format!("/storage/v1/b/test-bucket/o/{name}");
        let metadata = self.get(&path).json();
        let media = self.get(&format!("{path}?alt=media"));
        assert_eq!(media.status, 200);
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
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Reply {
    fn read(stream: TcpStream) -> Self {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0, "HTTP headers");
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.trim_end().split_once(':').unwrap();
            headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
        }
        let mut body = Vec::new();
        if !matches!(status, 204 | 304) {
            if headers
                .get("transfer-encoding")
                .is_some_and(|v| v == "chunked")
            {
                loop {
                    line.clear();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0, "chunk length");
                    let length = usize::from_str_radix(line.trim(), 16).unwrap();
                    if length == 0 {
                        break;
                    }
                    let start = body.len();
                    body.resize(start + length, 0);
                    reader.read_exact(&mut body[start..]).unwrap();
                    let mut crlf = [0; 2];
                    reader.read_exact(&mut crlf).unwrap();
                    assert_eq!(&crlf, b"\r\n");
                }
            } else {
                body.resize(headers["content-length"].parse().unwrap(), 0);
                reader.read_exact(&mut body).unwrap();
            }
        }
        Self {
            status,
            headers,
            body,
        }
    }

    fn json(&self) -> Value {
        assert_eq!(self.status, 200, "{}", String::from_utf8_lossy(&self.body));
        serde_json::from_slice(&self.body).unwrap()
    }

    fn error(&self, status: u16) -> Value {
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
        let error = self.error(412);
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
                assert_eq!(reply.status, 204, "{method} {condition}");
                assert!(reply.body.is_empty());
                server
                    .get(&format!("/storage/v1/b/test-bucket/o/{name}"))
                    .error(404);
                server
                    .get(&format!("/storage/v1/b/test-bucket/o/{name}?alt=media"))
                    .error(404);
            } else {
                let result = reply.json();
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
            let before = server.get(&path).json();
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
                reply.condition_failed();
                assert_eq!(server.get(&path).json(), before, "{method} {condition}");
            } else if method == "DELETE" {
                assert_eq!(reply.status, 204);
                assert!(reply.body.is_empty());
                server.get(&path).error(404);
            } else {
                let after = reply.json();
                assert_eq!(server.get(&path).json(), after);
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
            .json();
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
        assert_eq!(initiation.status, 200);
        let path = initiation.headers["location"]
            .strip_prefix("http://127.0.0.1:4443")
            .unwrap();
        let uploaded = server.request("PUT", path,
            &format!("Content-Type: application/octet-stream\r\nContent-Range: bytes 0-8/9\r\nX-Goog-Hash: {hash}\r\n"), HASH_BYTES,
        ).json();
        assert_eq!(uploaded["md5Hash"], MD5);
        assert_eq!(uploaded["crc32c"], CRC32C);
        assert_eq!(uploaded["size"], HASH_BYTES.len().to_string());
        assert_eq!(server.snapshot(name), (uploaded, HASH_BYTES.to_vec()));
    }
    server.stop();
}

#[test]
fn basic_contract_media_hash_matrix_validates_each_checksum_and_preserves_failed_overwrites() {
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
            .json();
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
        let name = format!("invalid-{index}");
        let headers = format!("Content-Type: application/octet-stream\r\nX-Goog-Hash: {hash}\r\n");
        server.upload(&name, &headers, HASH_BYTES).error(400);
        server
            .get(&format!("/storage/v1/b/test-bucket/o/{name}"))
            .error(404);
        server
            .get(&format!("/storage/v1/b/test-bucket/o/{name}?alt=media"))
            .error(404);
        let before = server.seed_object(&name);
        server.upload(&name, &headers, HASH_BYTES).error(400);
        assert_eq!(server.snapshot(&name), before, "{hash}");
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
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=bad-hash".to_owned(),
            "Content-Type: application/octet-stream\r\nX-Goog-Hash: md5=AAAAAAAAAAAAAAAAAAAAAA==\r\n",
            HASH_BYTES,
            400,
        ),
    ] {
        let expected = server.request(method, &path, headers, body).error(status);
        for selector in ["name", "*"] {
            let separator = if path.contains('?') { '&' } else { '?' };
            let selected = server
                .request(
                    method,
                    &format!("{path}{separator}fields={selector}"),
                    headers,
                    body,
                )
                .error(status);
            assert_eq!(selected, expected, "{method} {path} fields={selector}");
        }
    }
    assert_eq!(server.get("/storage/v1/b/test-bucket").json(), bucket);
    assert_eq!(server.snapshot("existing"), object);
    server
        .get("/storage/v1/b/test-bucket/o/bad-hash")
        .error(404);
    server.stop();
}

#[test]
fn basic_contract_strict_denials_keep_complete_json_envelopes_with_fields_selection() {
    let dir = TestDir::new();
    let identities = dir.0.join("identities.json");
    std::fs::write(&identities, json!({"identities":[
        {"id":"admin@example.invalid","tokens":["admin-token"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"denied@example.invalid","tokens":["denied-token"]}
    ]}).to_string()).unwrap();
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
        .json();
    server.upload("existing", admin, OLD_BYTES).json();
    for path in [
        "/storage/v1/b/test-bucket",
        "/storage/v1/b/test-bucket/o/existing",
    ] {
        for (headers, status) in [
            ("", 401),
            ("Authorization: Bearer invalid-token\r\n", 401),
            ("Authorization: Bearer denied-token\r\n", 403),
        ] {
            let expected = server.request("GET", path, headers, b"").error(status);
            for selector in ["name", "*"] {
                let selected = server
                    .request("GET", &format!("{path}?fields={selector}"), headers, b"")
                    .error(status);
                assert_eq!(selected, expected);
            }
        }
    }
    server.stop();
}
