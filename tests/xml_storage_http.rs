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
    stdout: BufReader<ChildStdout>,
    address: String,
}

impl Server {
    fn start(data: &TestDir, options: &[&str]) -> Self {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--connection-timeout-ms",
                "20000",
                "--api-base-url",
                "http://localhost:4443",
                "--data-dir",
            ])
            .arg(&data.0)
            .args(options)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            stdout,
            address: ready["listen"].as_str().unwrap().into(),
        }
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &str) -> (u16, String) {
        self.request_host("localhost", method, path, headers, body)
    }

    fn request_host(
        &self,
        host: &str,
        method: &str,
        path: &str,
        headers: &str,
        body: &str,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()).unwrap();
        let raw =
            common::read_http_response(&mut BufReader::new(stream), method == "HEAD").unwrap();
        let raw = String::from_utf8(raw).unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        (
            headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            body.into(),
        )
    }

    fn bucket(&self, auth: &str, name: &str) {
        let body = json!({"name":name,"versioning":{"enabled":true}}).to_string();
        let response = self.request(
            "POST",
            "/storage/v1/b?project=local",
            &format!("{auth}Content-Type: application/json\r\n"),
            &body,
        );
        assert_eq!(response.0, 200, "{}", response.1);
    }

    fn put(&self, auth: &str, name: &str, body: &str) -> i64 {
        let response = self.request(
            "POST",
            &format!(
                "/upload/storage/v1/b/list-bucket/o?uploadType=media&name={}",
                encode(name)
            ),
            auth,
            body,
        );
        assert_eq!(response.0, 200, "{}", response.1);
        serde_json::from_str::<Value>(&response.1).unwrap()["generation"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    fn list(&self, query: &str) -> String {
        let response = self.request("GET", &format!("/list-bucket?{query}"), "", "");
        assert_eq!(response.0, 200, "{query}: {}", response.1);
        response.1
    }

    fn resource(&self, auth: &str, bucket: &str, name: &str) -> Value {
        let (status, body) = self.request(
            "GET",
            &format!("/storage/v1/b/{bucket}/o/{}", encode(name)),
            auth,
            "",
        );
        assert_eq!(status, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

fn encode(value: &str) -> String {
    value.bytes().map(|byte| format!("%{byte:02X}")).collect()
}

fn base64(bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for shift in [18, 12, 6, 0] {
            encoded.push(
                if (shift == 0 && chunk.len() < 3) || (shift == 6 && chunk.len() < 2) {
                    '='
                } else {
                    alphabet[((value >> shift) & 63) as usize] as char
                },
            );
        }
    }
    encoded
}

fn signed_v2(resource: &str) -> String {
    use ring::{rand::SystemRandom, signature};
    use tokio_rustls::rustls::pki_types::{PrivateKeyDer, pem::PemObject};
    let private = include_bytes!("../fixtures/signing/test-only-private.pem");
    let key = PrivateKeyDer::from_pem_slice(private).unwrap();
    let key = signature::RsaKeyPair::from_pkcs8(key.secret_der()).unwrap();
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 60;
    let message = format!("GET\n\n\n{expires}\n{resource}");
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        message.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!(
        "{resource}{}GoogleAccessId=admin%40example.invalid&Expires={expires}&Signature={}",
        if resource.contains('?') { '&' } else { '?' },
        encode(&base64(&signature))
    )
}

fn key_headers(prefix: &str, byte: u8) -> String {
    let bytes = [byte; 32];
    format!(
        "{prefix}algorithm: AES256\r\n{prefix}key: {}\r\n{prefix}key-sha256: {}\r\n",
        base64(&bytes),
        base64(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref())
    )
}

fn elements<'a>(body: &'a str, tag: &str) -> Vec<&'a str> {
    body.split(&format!("<{tag}>"))
        .skip(1)
        .map(|part| part.split_once(&format!("</{tag}>")).unwrap().0)
        .collect()
}

#[test]
fn xml_path_style_decodes_bucket_once_and_preserves_object_names() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    let name = "dir/日本語 +%?.txt";
    let path = format!("/list%2Dbucket/{}", encode(name));
    let response = server.request("PUT", &path, "", "encoded body");
    assert_eq!(response.0, 200, "{}", response.1);
    for bucket in ["list-bucket", "list%2Dbucket", "list%2dbucket"] {
        assert_eq!(
            server.request("GET", &format!("/{bucket}/{}", encode(name)), "", ""),
            (200, "encoded body".into())
        );
        let response = server.request("GET", &format!("/{bucket}?prefix=dir"), "", "");
        assert_eq!(response.0, 200, "{}", response.1);
        assert_eq!(elements(&response.1, "Key"), [name]);
    }
    let copied = "/list%2Dbucket/dir%2Fcopy.txt";
    let headers = format!("x-goog-copy-source: {path}\r\nx-goog-if-generation-match: 0\r\n");
    let response = server.request("PUT", copied, &headers, "");
    assert_eq!(response.0, 200, "{}", response.1);
    assert_eq!(server.request("PUT", copied, &headers, "").0, 412);
    assert_eq!(
        server.request("GET", copied, "", ""),
        (200, "encoded body".into())
    );
    let original = server.resource("", "list-bucket", name);
    for (bucket, status) in [
        ("list%252Dbucket", 404),
        ("list%2Fbucket", 404),
        ("list%GGbucket", 400),
        ("list%E9%9B%AAbucket", 404),
        ("list%FFbucket", 400),
    ] {
        for method in ["GET", "PUT"] {
            let response = server.request(method, &format!("/{bucket}/{}", encode(name)), "", "");
            assert_eq!(response.0, status, "{method} {bucket}: {response:?}");
        }
    }
    assert_eq!(server.resource("", "list-bucket", name), original);
    let listed = server.list("prefix=dir");
    assert_eq!(elements(&listed, "Key").len(), 2);
}

#[test]
fn xml_bucket_list_v1_v2_pages_prefixes_markers_and_empty_results() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    for name in ["a", "a/", "a/one", "a/two", "b", "c"] {
        server.put("", name, name);
    }
    for v2 in [false, true] {
        let base = if v2 {
            "list-type=2&delimiter=/&max-keys=1"
        } else {
            "delimiter=/&max-keys=1"
        };
        let mut query = base.to_owned();
        let mut names = Vec::new();
        for _ in 0..10 {
            let body = server.list(&query);
            names.extend(elements(&body, "Key").into_iter().map(str::to_owned));
            if body.contains("<CommonPrefixes>") {
                assert!(body.contains("<CommonPrefixes><Prefix>a/</Prefix></CommonPrefixes>"));
                names.push("a/".into());
            }
            if body.contains("<IsTruncated>false</IsTruncated>") {
                break;
            }
            let tag = if v2 {
                "NextContinuationToken"
            } else {
                "NextMarker"
            };
            let token = elements(&body, tag)[0];
            if v2 {
                assert!(token.starts_with("xml2."));
            }
            query = format!(
                "{base}&{}={}",
                if v2 { "continuation-token" } else { "marker" },
                encode(token)
            );
        }
        assert_eq!(names, ["a", "a/", "b", "c"]);
    }
    assert_eq!(
        elements(&server.list("marker=a/one&delimiter=/"), "Key"),
        ["b", "c"]
    );
    assert_eq!(
        elements(&server.list("list-type=2&start-after=b"), "Key"),
        ["c"]
    );
    for query in ["prefix=missing", "marker=z", "list-type=2&prefix=missing"] {
        let body = server.list(query);
        assert!(elements(&body, "Key").is_empty());
        assert!(body.contains("<IsTruncated>false</IsTruncated>"));
    }
    let body = server.list("prefix=a/&delimiter=/");
    assert_eq!(elements(&body, "Key"), ["a/", "a/one", "a/two"]);
}

#[test]
fn xml_bucket_list_encodes_once_and_keeps_object_namespace_collisions() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    for name in ["a &<\"'雪", "a%2Fb", "storage/v1/b", "_internal/ready"] {
        server.put("", name, "body");
    }
    let raw = server.list("");
    assert!(raw.contains("a &amp;&lt;&quot;&apos;雪"));
    let encoded = server.list("encoding-type=url");
    assert!(encoded.contains("<EncodingType>url</EncodingType>"));
    assert!(encoded.contains("a%20%26%3C%22%27%E9%9B%AA"));
    assert!(encoded.contains("a%252Fb"));
    assert!(encoded.contains("storage%2Fv1%2Fb"));
    assert!(encoded.contains("_internal%2Fready"));
    assert_eq!(
        server.request("GET", "/list-bucket/storage/v1/b", "", ""),
        (200, "body".into())
    );
}

#[test]
fn xml_bucket_list_versions_resume_by_generation_and_survive_restart() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    let first = server.put("", "same", "old");
    let second = server.put("", "same", "new");
    let page = server.list("versions&max-keys=1");
    assert!(page.contains("<Version><Key>same</Key>"));
    assert_eq!(elements(&page, "Generation"), [first.to_string()]);
    assert_eq!(elements(&page, "IsLatest"), ["false"]);
    assert_eq!(elements(&page, "NextGenerationMarker"), [first.to_string()]);
    drop(server);
    let server = Server::start(&data, &[]);
    let page = server.list(&format!("versions&marker=same&generation-marker={first}"));
    assert_eq!(elements(&page, "Generation"), [second.to_string()]);
    assert_eq!(elements(&page, "IsLatest"), ["true"]);
    let (_, interop) = server.request(
        "GET",
        "/list-bucket?versions&max-keys=1",
        "x-goog-interop-list-objects-format: enabled\r\n",
        "",
    );
    assert!(interop.contains("<ListVersionsResult "));
    assert_eq!(elements(&interop, "VersionId"), [first.to_string()]);
    assert_eq!(
        elements(&interop, "NextVersionIdMarker"),
        [first.to_string()]
    );
}

#[test]
fn xml_bucket_list_rejects_invalid_scopes_options_and_preserves_objects() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    server.bucket("", "other-bucket");
    let generation = server.put("", "a", "a");
    server.put("", "b", "b");
    let page = server.list("list-type=2&max-keys=1");
    let token = elements(&page, "NextContinuationToken")[0];
    for query in [
        "list-type=3",
        "max-keys=-1",
        "max-keys=0",
        "max-keys=4294967296",
        "encoding-type=xml",
        "acl",
        "list-type=2&marker=a",
        "fetch-owner=false",
        "list-type=2&fetch-owner=true",
        "generation-marker=1",
        "versions&marker=a&generation-marker=0",
        "prefix=a&prefix=b",
        "list-type=2&continuation-token=bad",
    ] {
        let response = server.request("GET", &format!("/list-bucket?{query}"), "", "");
        assert_eq!(response.0, 400, "{query}: {}", response.1);
        assert!(response.1.contains("<Code>InvalidArgument</Code>"));
    }
    for path in [
        format!("/other-bucket?list-type=2&continuation-token={token}"),
        format!("/list-bucket?list-type=2&prefix=a&continuation-token={token}"),
        format!("/list-bucket?list-type=2&versions&continuation-token={token}"),
        format!("/storage/v1/b/list-bucket/o?pageToken={token}"),
    ] {
        assert_eq!(server.request("GET", &path, "", "").0, 400, "{path}");
    }
    let body = server.list("");
    assert_eq!(elements(&body, "Generation")[0], generation.to_string());
    assert_eq!(
        server.request("GET", "/list-bucket/a", "", ""),
        (200, "a".into())
    );
    let response = server.request("GET", "/missing-bucket?list-type=2", "", "");
    assert_eq!(response.0, 404);
    assert!(response.1.contains("<Code>NoSuchBucket</Code>"));
}

#[test]
fn xml_bucket_list_enforces_strict_list_permission_and_revocation() {
    let data = TestDir::new();
    let config = TestDir::new();
    let registry = config.0.join("identities.json");
    std::fs::write(&registry, json!({"identities":[
        {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"creator@example.invalid","tokens":["creator"],"project_roles":{"local":["roles/storage.objectCreator"]}},
        {"id":"reader@example.invalid","tokens":["reader"]}
    ]}).to_string()).unwrap();
    let server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    let admin = "Authorization: Bearer admin\r\n";
    let reader = "Authorization: Bearer reader\r\n";
    server.bucket(admin, "list-bucket");
    server.put(admin, "private", "private");
    for (headers, status) in [
        ("", 401),
        (reader, 403),
        ("Authorization: Bearer creator\r\n", 403),
    ] {
        assert_eq!(
            server
                .request("GET", "/list-bucket?list-type=2", headers, "")
                .0,
            status
        );
    }
    let policy = json!({"bindings":[{"role":"roles/storage.objectViewer","members":["user:reader@example.invalid"]}]}).to_string();
    assert_eq!(
        server
            .request("PUT", "/storage/v1/b/list-bucket/iam", admin, &policy)
            .0,
        200
    );
    let response = server.request("GET", "/list-bucket?list-type=2", reader, "");
    assert_eq!(response.0, 200, "{}", response.1);
    assert_eq!(elements(&response.1, "Key"), ["private"]);
    assert_eq!(
        server
            .request(
                "PUT",
                "/storage/v1/b/list-bucket/iam",
                admin,
                r#"{"bindings":[]}"#
            )
            .0,
        200
    );
    assert_eq!(
        server
            .request("GET", "/list-bucket?list-type=2", reader, "")
            .0,
        403
    );
}

#[test]
fn xml_bucket_list_v2_signatures_reject_host_transfer_and_bind_versions() {
    let data = TestDir::new();
    let config = TestDir::new();
    let registry = config.0.join("identities.json");
    std::fs::write(
        &registry,
        json!({"identities":[{
            "id":"admin@example.invalid", "tokens":["admin"],
            "public_keys":[include_str!("../fixtures/signing/test-only-public.pem")],
            "project_roles":{"local":["roles/storage.admin"]}
        }]})
        .to_string(),
    )
    .unwrap();
    let server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
            "--xml-virtual-host-domains",
            "storage.invalid",
        ],
    );
    let admin = "Authorization: Bearer admin\r\n";
    for bucket in ["list-bucket", "other-bucket"] {
        server.bucket(admin, bucket);
    }
    server.put(admin, "history", "old");
    server.put(admin, "history", "new");
    for bucket in ["list%2Dbucket", "list%2dbucket"] {
        let signed = signed_v2(&format!("/{bucket}/history"));
        assert_eq!(server.request("GET", &signed, "", ""), (200, "new".into()));
        let changed = signed.replacen(bucket, "list-bucket", 1);
        let response = server.request("GET", &changed, "", "");
        assert_eq!(response.0, 403, "{}", response.1);
        assert!(response.1.contains("<Code>SignatureDoesNotMatch</Code>"));
    }
    let ordinary = signed_v2("/list-bucket");
    let response = server.request("GET", &ordinary, "", "");
    assert_eq!(response.0, 200, "{}", response.1);
    assert_eq!(elements(&response.1, "Generation").len(), 1);
    assert_eq!(
        server
            .request("GET", &format!("{ordinary}&versions"), "", "")
            .0,
        403
    );
    for resource in ["/list-bucket?versions", "/list-bucket?versions=true"] {
        let signed = signed_v2(resource);
        let response = server.request("GET", &signed, "", "");
        assert_eq!(response.0, 200, "{}", response.1);
        assert_eq!(elements(&response.1, "Generation").len(), 2);
        let removed = signed
            .replace("versions=true&", "")
            .replace("versions&", "");
        assert_eq!(server.request("GET", &removed, "", "").0, 403);
        let changed = if resource.ends_with("=true") {
            signed.replace("versions=true", "versions=false")
        } else {
            signed.replace("versions&", "versions=true&")
        };
        assert_eq!(server.request("GET", &changed, "", "").0, 403);
    }
    let virtual_url = signed_v2("/");
    for host in [
        "list-bucket.storage.invalid",
        "other-bucket.storage.invalid",
    ] {
        let response = server.request_host(host, "GET", &virtual_url, "", "");
        assert_eq!(response.0, 403, "{host}: {}", response.1);
        assert!(response.1.contains("<Code>SignatureDoesNotMatch</Code>"));
        assert!(!response.1.contains("<Key>"));
    }
    let virtual_url = signed_v2("/list-bucket/").replacen("/list-bucket/?", "/?", 1);
    let response = server.request_host("list-bucket.storage.invalid", "GET", &virtual_url, "", "");
    assert_eq!(response.0, 400, "{}", response.1);
    assert!(response.1.contains("<Code>InvalidArgument</Code>"));
    let response = server.request_host("other-bucket.storage.invalid", "GET", &virtual_url, "", "");
    assert_eq!(response.0, 403, "{}", response.1);
    assert!(response.1.contains("<Code>SignatureDoesNotMatch</Code>"));
    assert!(!response.1.contains("<Key>"));
    assert_eq!(
        server
            .request(
                "GET",
                &ordinary.replace("/list-bucket?", "/other-bucket?"),
                "",
                ""
            )
            .0,
        403
    );
}

#[test]
fn xml_copy_selects_generation_and_copies_or_replaces_metadata_with_integrity() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    let old = server.put("", "source /%雪", "old bytes");
    let source_path = format!("/list-bucket/{}", encode("source /%雪"));
    let response = server.request("PUT", &source_path,
        "Content-Type: text/plain\r\nCache-Control: private,max-age=15\r\nContent-Disposition: attachment\r\nx-goog-meta-original: kept\r\n", "hello");
    assert_eq!(response.0, 200, "{}", response.1);
    let original = server.resource("", "list-bucket", "source /%雪");
    let generation = original["generation"].as_str().unwrap();
    let headers = format!(
        "x-goog-copy-source: {source_path}\r\nx-goog-copy-source-generation: {generation}\r\nx-goog-copy-source-if-generation-match: {generation}\r\nx-goog-copy-source-if-metageneration-match: 1\r\nx-goog-copy-source-if-match: \"5d41402abc4b2a76b9719d911017c592\"\r\nx-goog-if-generation-match: 0\r\nContent-MD5: XUFAKrxLKna5cZ2REBfFkg==\r\nx-goog-hash: crc32c=mnG7TA==\r\n"
    );
    let response = server.request("PUT", "/list-bucket/copied", &headers, "");
    assert_eq!(response.0, 200, "{}", response.1);
    assert!(response.1.contains("<CopyObjectResult>"));
    assert_eq!(
        elements(&response.1, "LastModified"),
        [original["updated"].as_str().unwrap()]
    );
    let copied = server.resource("", "list-bucket", "copied");
    for field in [
        "contentType",
        "cacheControl",
        "contentDisposition",
        "metadata",
        "size",
        "md5Hash",
        "crc32c",
        "storageClass",
    ] {
        assert_eq!(copied[field], original[field], "{field}");
    }
    assert_ne!(copied["generation"], original["generation"]);
    assert_eq!(
        server.request("GET", "/list-bucket/copied", "", ""),
        (200, "hello".into())
    );
    let replacement = format!(
        "x-goog-copy-source: {source_path}\r\nx-goog-metadata-directive: REPLACE\r\nContent-Type: application/custom\r\nx-goog-meta-new: replaced\r\nx-goog-storage-class: NEARLINE\r\n"
    );
    assert_eq!(
        server
            .request("PUT", "/list-bucket/replaced", &replacement, "")
            .0,
        200
    );
    let replaced = server.resource("", "list-bucket", "replaced");
    assert_eq!(replaced["contentType"], "application/custom");
    assert_eq!(replaced["storageClass"], "NEARLINE");
    assert_eq!(replaced["metadata"], json!({"new":"replaced"}));
    assert!(replaced.get("cacheControl").is_none());
    assert!(replaced.get("contentDisposition").is_none());
    let old_headers =
        format!("x-goog-copy-source: {source_path}\r\nx-goog-copy-source-generation: {old}\r\n");
    assert_eq!(
        server
            .request("PUT", "/list-bucket/old", &old_headers, "")
            .0,
        200
    );
    assert_eq!(
        server.request("GET", "/list-bucket/old", "", ""),
        (200, "old bytes".into())
    );
    assert_eq!(server.resource("", "list-bucket", "source /%雪"), original);
    drop(server);
    let server = Server::start(&data, &[]);
    assert_eq!(
        server.request("GET", "/list-bucket/copied", "", ""),
        (200, "hello".into())
    );
    assert_eq!(server.resource("", "list-bucket", "copied"), copied);
}

#[test]
fn xml_copy_failed_conditions_digests_and_unsupported_inputs_preserve_destination() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    let generation = server.put("", "source", "hello");
    server.put("", "destination", "kept");
    let original = server.resource("", "list-bucket", "destination");
    for (extra, status, code) in [
        (
            "x-goog-if-generation-match: 0\r\n",
            412,
            "PreconditionFailed",
        ),
        (
            "x-goog-if-metageneration-match: 999\r\n",
            412,
            "PreconditionFailed",
        ),
        (
            "x-goog-copy-source-if-generation-match: 1\r\n",
            412,
            "PreconditionFailed",
        ),
        (
            "x-goog-copy-source-if-match: mismatch\r\n",
            412,
            "PreconditionFailed",
        ),
        (
            "x-goog-copy-source-if-none-match: 5d41402abc4b2a76b9719d911017c592\r\n",
            412,
            "PreconditionFailed",
        ),
        (
            "Content-MD5: AAAAAAAAAAAAAAAAAAAAAA==\r\n",
            400,
            "BadDigest",
        ),
        ("x-goog-hash: crc32c=AAAAAA==\r\n", 400, "BadDigest"),
        ("Content-MD5: malformed\r\n", 400, "InvalidDigest"),
        (
            "x-goog-metadata-directive: INVALID\r\n",
            400,
            "InvalidArgument",
        ),
        (
            "x-goog-copy-source-if-metageneration-match: 1\r\n",
            400,
            "InvalidArgument",
        ),
        (
            "x-goog-copy-source-generation: 0\r\n",
            400,
            "InvalidArgument",
        ),
        ("x-goog-copy-source-if-match: *\r\n", 400, "InvalidArgument"),
        (
            "x-goog-copy-source-if-modified-since: Fri, 19 Feb 2010 14:05:04 GMT\r\n",
            400,
            "InvalidArgument",
        ),
        (
            "x-goog-gcs-idempotency-token: retry\r\n",
            400,
            "InvalidArgument",
        ),
        (
            "x-goog-copy-source: /list-bucket/source\r\n",
            400,
            "ExcessHeaderValues",
        ),
    ] {
        let response = server.request(
            "PUT",
            "/list-bucket/destination",
            &format!("x-goog-copy-source: /list-bucket/source\r\n{extra}"),
            "",
        );
        assert_eq!(response.0, status, "{extra}: {}", response.1);
        assert!(
            response.1.contains(&format!("<Code>{code}</Code>")),
            "{extra}: {}",
            response.1
        );
        assert_eq!(
            server.resource("", "list-bucket", "destination"),
            original,
            "{extra}"
        );
    }
    let response = server.request("PUT", "/list-bucket/destination", &format!("x-goog-copy-source: /list-bucket/source\r\nx-goog-copy-source-generation: {generation}\r\nx-goog-copy-source-if-metageneration-match: 2\r\n"), "");
    assert_eq!(response.0, 412, "{}", response.1);
    assert_eq!(
        server
            .request(
                "PUT",
                "/list-bucket/source",
                "x-goog-copy-source: /list-bucket/source\r\nx-goog-metadata-directive: REPLACE\r\n",
                ""
            )
            .0,
        400
    );
    for source in [
        "",
        "/list-bucket",
        "/list-bucket/%ZZ",
        "/list-bucket/name?generation=1",
    ] {
        assert_eq!(
            server
                .request(
                    "PUT",
                    "/list-bucket/destination",
                    &format!("x-goog-copy-source: {source}\r\n"),
                    ""
                )
                .0,
            400
        );
    }
    assert_eq!(
        server
            .request(
                "PUT",
                "/list-bucket/destination",
                "x-goog-copy-source: /list-bucket/missing\r\n",
                ""
            )
            .0,
        404
    );
    assert_eq!(
        server.request("GET", "/list-bucket/destination", "", ""),
        (200, "kept".into())
    );
    assert_eq!(server.resource("", "list-bucket", "destination"), original);
}

#[test]
fn xml_copy_create_only_condition_has_one_concurrent_winner() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    server.put("", "source", "hello");
    let mut statuses = std::thread::scope(|scope| {
        let jobs: Vec<_> = (0..4).map(|_| scope.spawn(|| server.request("PUT", "/list-bucket/race", "x-goog-copy-source: /list-bucket/source\r\nx-goog-if-generation-match: 0\r\n", "").0)).collect();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 412, 412, 412]);
    assert_eq!(
        server.request("GET", "/list-bucket/race", "", ""),
        (200, "hello".into())
    );
    let body = server.list("prefix=race&versions");
    assert_eq!(elements(&body, "Generation").len(), 1);
}

#[test]
fn xml_copy_requires_source_get_destination_create_and_overwrite_delete() {
    let data = TestDir::new();
    let config = TestDir::new();
    let registry = config.0.join("identities.json");
    std::fs::write(&registry, json!({"identities":[
        {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
        {"id":"creator@example.invalid","tokens":["creator"],"project_roles":{"local":["roles/storage.objectCreator"]}},
        {"id":"reader@example.invalid","tokens":["reader"],"project_roles":{"local":["roles/storage.objectViewer"]}}
    ]}).to_string()).unwrap();
    let server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    let admin = "Authorization: Bearer admin\r\n";
    let creator = "Authorization: Bearer creator\r\n";
    server.bucket(admin, "list-bucket");
    server.bucket(admin, "other-bucket");
    server.put(admin, "source", "hello");
    let headers = "x-goog-copy-source: /list-bucket/source\r\n";
    for (auth, status) in [
        ("", 401),
        (creator, 403),
        ("Authorization: Bearer reader\r\n", 403),
        ("Authorization: Bearer unknown\r\n", 400),
    ] {
        assert_eq!(
            server
                .request(
                    "PUT",
                    "/other-bucket/copied",
                    &format!("{auth}{headers}"),
                    ""
                )
                .0,
            status
        );
    }
    let policy = json!({"bindings":[{"role":"roles/storage.objectViewer","members":["user:creator@example.invalid"]}]}).to_string();
    assert_eq!(
        server
            .request("PUT", "/storage/v1/b/list-bucket/iam", admin, &policy)
            .0,
        200
    );
    let response = server.request(
        "PUT",
        "/other-bucket/copied",
        &format!("{creator}{headers}"),
        "",
    );
    assert_eq!(response.0, 200, "{}", response.1);
    let copied = server.resource(admin, "other-bucket", "copied");
    assert_eq!(
        server
            .request(
                "PUT",
                "/other-bucket/copied",
                &format!("{creator}{headers}"),
                ""
            )
            .0,
        403
    );
    assert_eq!(server.resource(admin, "other-bucket", "copied"), copied);
    assert_eq!(
        server
            .request(
                "PUT",
                "/storage/v1/b/list-bucket/iam",
                admin,
                r#"{"bindings":[]}"#
            )
            .0,
        200
    );
    assert_eq!(
        server
            .request(
                "PUT",
                "/other-bucket/revoked",
                &format!("{creator}{headers}"),
                ""
            )
            .0,
        403
    );
    assert_eq!(
        server.request("GET", "/other-bucket/copied", admin, ""),
        (200, "hello".into())
    );
}

#[test]
fn xml_copy_checks_source_customer_key_and_sets_destination_key() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    let original_key = key_headers("x-goog-encryption-", 7);
    assert_eq!(
        server
            .request("PUT", "/list-bucket/encrypted", &original_key, "hello")
            .0,
        200
    );
    server.put("", "destination", "kept");
    let original = server.resource("", "list-bucket", "destination");
    let source = "x-goog-copy-source: /list-bucket/encrypted\r\n";
    for key in [
        String::new(),
        key_headers("x-goog-copy-source-encryption-", 8),
    ] {
        let response = server.request(
            "PUT",
            "/list-bucket/destination",
            &format!("{source}{key}"),
            "",
        );
        assert_eq!(response.0, 400, "{}", response.1);
        assert_eq!(server.resource("", "list-bucket", "destination"), original);
    }
    let source_key = key_headers("x-goog-copy-source-encryption-", 7);
    let destination_key = key_headers("x-goog-encryption-", 8);
    let response = server.request(
        "PUT",
        "/list-bucket/destination",
        &format!("{source}{source_key}{destination_key}"),
        "",
    );
    assert_eq!(response.0, 200, "{}", response.1);
    assert_eq!(
        server.request("GET", "/list-bucket/destination", "", "").0,
        400
    );
    assert_eq!(
        server
            .request("GET", "/list-bucket/destination", &original_key, "")
            .0,
        400
    );
    assert_eq!(
        server.request("GET", "/list-bucket/destination", &destination_key, ""),
        (200, "hello".into())
    );
    assert_eq!(
        server.request("GET", "/list-bucket/encrypted", &original_key, ""),
        (200, "hello".into())
    );
}

#[test]
fn xml_copy_obeys_object_size_limit_and_preserves_existing_destination() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.bucket("", "list-bucket");
    server.put("", "source", "too large");
    server.put("", "destination", "kept");
    let original = server.resource("", "list-bucket", "destination");
    drop(server);
    let server = Server::start(&data, &["--max-object-bytes", "5"]);
    let response = server.request(
        "PUT",
        "/list-bucket/destination",
        "x-goog-copy-source: /list-bucket/source\r\n",
        "",
    );
    assert_eq!(response.0, 400, "{}", response.1);
    assert!(response.1.contains("<Code>EntityTooLarge</Code>"));
    assert_eq!(server.resource("", "list-bucket", "destination"), original);
    assert_eq!(
        server.request("GET", "/list-bucket/destination", "", ""),
        (200, "kept".into())
    );
}

#[test]
fn xml_copy_v4_signature_binds_source_and_destination_conditions() {
    let data = TestDir::new();
    let config = TestDir::new();
    let registry = config.0.join("identities.json");
    std::fs::write(
        &registry,
        json!({"identities":[{
            "id":"admin@example.invalid", "tokens":["admin"],
            "public_keys":[include_str!("../fixtures/signing/test-only-public.pem")],
            "project_roles":{"local":["roles/storage.admin"]}
        }]})
        .to_string(),
    )
    .unwrap();
    let server = Server::start(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    let admin = "Authorization: Bearer admin\r\n";
    server.bucket(admin, "list-bucket");
    server.put(admin, "source", "hello");
    server.put(admin, "other", "other");
    let mut headers = hyper::HeaderMap::new();
    headers.insert("x-goog-copy-source", "/list-bucket/source".parse().unwrap());
    headers.insert("x-goog-if-generation-match", "0".parse().unwrap());
    let url = gcs_mimic::auth::signature::sign_url(
        "http://localhost/list-bucket/copied",
        &hyper::Method::PUT,
        "admin@example.invalid",
        include_bytes!("../fixtures/signing/test-only-private.pem"),
        60,
        std::time::SystemTime::now(),
        headers,
    )
    .unwrap();
    let uri: hyper::Uri = url.parse().unwrap();
    let path = uri.path_and_query().unwrap().as_str();
    for headers in [
        "x-goog-copy-source: /list-bucket/other\r\nx-goog-if-generation-match: 0\r\n",
        "x-goog-copy-source: /list-bucket/source\r\nx-goog-if-generation-match: 1\r\n",
    ] {
        assert_eq!(server.request("PUT", path, headers, "").0, 403);
    }
    let response = server.request(
        "PUT",
        path,
        "x-goog-copy-source: /list-bucket/source\r\nx-goog-if-generation-match: 0\r\n",
        "",
    );
    assert_eq!(response.0, 200, "{}", response.1);
    assert_eq!(
        server.request("GET", "/list-bucket/copied", admin, ""),
        (200, "hello".into())
    );
    assert_eq!(
        server
            .request(
                "PUT",
                path,
                "x-goog-copy-source: /list-bucket/source\r\nx-goog-if-generation-match: 0\r\n",
                ""
            )
            .0,
        412
    );
}
