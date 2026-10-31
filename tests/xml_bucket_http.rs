mod common;

use common::TestDir;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::{Duration, SystemTime},
};

const ADMIN: &str = "Authorization: Bearer admin-token\r\n";
const VIEWER: &str = "Authorization: Bearer viewer-token\r\n";
const NONE: &str = "Authorization: Bearer no-roles-token\r\n";
const PROJECT: &str = "x-goog-project-id: project-a\r\n";
const PRIVATE: &[u8] = include_bytes!("../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../fixtures/signing/test-only-public.pem");

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
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
                "--data-dir",
            ])
            .arg(data.0.join("data"))
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

    fn request(&self, method: &str, path: &str, headers: &str, body: &str) -> Reply {
        self.host("localhost", method, path, headers, body)
    }

    fn host(&self, host: &str, method: &str, path: &str, headers: &str, body: &str) -> Reply {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}", body.len()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut raw = common::read_http_response(&mut reader, method == "HEAD").unwrap();
        // Read past the HEAD header boundary so an erroneously transmitted body cannot hide.
        reader.read_to_end(&mut raw).unwrap();
        let raw = String::from_utf8(raw).unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        Reply {
            status: headers.split_whitespace().nth(1).unwrap().parse().unwrap(),
            headers: headers.to_ascii_lowercase(),
            body: body.into(),
        }
    }

    fn create(&self, project: &str, name: &str) {
        let reply = self.request(
            "POST",
            &format!("/storage/v1/b?project={project}"),
            &format!("{ADMIN}Content-Type: application/json\r\n"),
            &json!({"name": name}).to_string(),
        );
        assert_eq!(reply.status, 200, "{}", reply.body);
    }

    fn sign(&self, method: hyper::Method, path: &str, headers: hyper::HeaderMap) -> hyper::Uri {
        gcs_mimic::auth::signature::sign_url(
            &format!("http://localhost{path}"),
            &method,
            "admin@example.invalid",
            PRIVATE,
            60,
            SystemTime::now(),
            headers,
        )
        .unwrap()
        .parse()
        .unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

fn strict(data: &TestDir) -> Server {
    let identities = data.0.join("identities.json");
    std::fs::write(&identities, json!({"identities":[
        {"id":"admin@example.invalid", "tokens":["admin-token"], "public_keys":[PUBLIC],
         "project_roles":{"project-a":["roles/storage.admin"],"project-b":["roles/storage.admin"]}},
        {"id":"viewer@example.invalid", "tokens":["viewer-token"],
         "project_roles":{"project-a":["roles/storage.objectViewer"]}},
        {"id":"no-roles@example.invalid", "tokens":["no-roles-token"]},
        {"id":"editor@example.invalid", "tokens":["editor-token"],
         "project_roles":{"project-a":["roles/storage.editor"]}}
    ]}).to_string()).unwrap();
    Server::start(
        data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
            "--xml-virtual-host-domains",
            "storage.local",
        ],
    )
}

fn names(body: &str) -> Vec<&str> {
    body.split("<Name>")
        .skip(1)
        .map(|tail| tail.split_once("</Name>").unwrap().0)
        .collect()
}

#[test]
fn xml_service_lists_only_selected_project_in_order_with_persisted_creation_dates() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.create("project-a", "zulu-bucket");
    server.create("project-b", "hidden-bucket");
    server.create("project-a", "alpha-bucket");
    let reply = server.request("GET", "/", PROJECT, "");
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(reply.headers.contains("content-type: application/xml"));
    assert!(
        reply
            .body
            .contains("<ListAllMyBucketsResult xmlns=\"http://doc.s3.amazonaws.com/2006-03-01\">")
    );
    assert_eq!(names(&reply.body), ["alpha-bucket", "zulu-bucket"]);
    let metadata = server.request("GET", "/storage/v1/b/alpha-bucket", "", "");
    let metadata: Value = serde_json::from_str(&metadata.body).unwrap();
    assert!(reply.body.contains(&format!(
        "<CreationDate>{}</CreationDate>",
        metadata["timeCreated"].as_str().unwrap()
    )));
    let empty = server.request("GET", "/", "x-goog-project-id: empty\r\n", "");
    assert_eq!(empty.status, 200);
    assert!(names(&empty.body).is_empty());
    drop(server);
    let server = Server::start(&data, &[]);
    assert_eq!(server.request("GET", "/", PROJECT, "").body, reply.body);
}

#[test]
fn xml_service_bounds_and_validates_project_headers_query_and_body() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    for (path, headers, body) in [
        ("/", "", ""),
        ("/", "x-goog-project-id: \r\n", ""),
        ("/", "x-goog-project-id: project a\r\n", ""),
        ("/", "x-goog-project-id: a\r\nx-goog-project-id: b\r\n", ""),
        ("/?project=project-a", PROJECT, ""),
        ("/?acl", PROJECT, ""),
        ("/?x=1&x=2", PROJECT, ""),
        ("/", PROJECT, "x"),
        (
            "/",
            "x-goog-project-id: project-a\r\nx-goog-user-project: billed\r\n",
            "",
        ),
        (
            "/",
            "x-goog-project-id: project-a\r\nx-http-method-override: GET\r\n",
            "",
        ),
    ] {
        let reply = server.request("GET", path, headers, body);
        assert_eq!(reply.status, 400, "{path} {headers}: {}", reply.body);
        assert!(reply.body.contains("<Error>"), "{}", reply.body);
    }
}

#[test]
fn xml_service_strict_authorization_and_signed_project_binding() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "alpha-bucket");
    for (auth, status) in [("", 401), (NONE, 403), (VIEWER, 403), (ADMIN, 200)] {
        assert_eq!(
            server
                .request("GET", "/", &format!("{auth}{PROJECT}"), "")
                .status,
            status
        );
    }
    let mut headers = hyper::HeaderMap::new();
    headers.insert("x-goog-project-id", "project-a".parse().unwrap());
    let url = server.sign(hyper::Method::GET, "/", headers);
    let path = url.path_and_query().unwrap().as_str();
    assert_eq!(server.request("GET", path, PROJECT, "").status, 200);
    let changed = server.request("GET", path, "x-goog-project-id: project-b\r\n", "");
    assert_eq!(changed.status, 403);
    assert!(changed.body.contains("SignatureDoesNotMatch"));
}

#[test]
fn xml_head_bucket_checks_access_and_sends_no_body_on_success_or_error() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "head-bucket");
    for (path, auth, body, status) in [
        ("/head-bucket", ADMIN, "", 200),
        ("/head-bucket/", ADMIN, "", 200),
        ("/head%2Dbucket", ADMIN, "", 200),
        ("/head%252Dbucket", ADMIN, "", 404),
        ("/missing-bucket", ADMIN, "", 404),
        ("/head-bucket", VIEWER, "", 403),
        ("/head-bucket", NONE, "", 403),
        ("/head-bucket", "", "", 401),
        ("/head-bucket?acl", ADMIN, "", 400),
        ("/head-bucket?generation=1", ADMIN, "", 400),
        ("/head-bucket", ADMIN, "body", 400),
    ] {
        let reply = server.request("HEAD", path, auth, body);
        assert_eq!(reply.status, status, "{path}: {}", reply.headers);
        assert!(reply.body.is_empty(), "{path}: {}", reply.body);
    }
    let reply = server.host("head-bucket.storage.local", "HEAD", "/", ADMIN, "");
    assert_eq!(reply.status, 200);
    assert!(reply.body.is_empty());
    let url = server.sign(
        hyper::Method::HEAD,
        "/head%2Dbucket",
        hyper::HeaderMap::new(),
    );
    assert_eq!(
        server
            .request("HEAD", url.path_and_query().unwrap().as_str(), "", "")
            .status,
        200
    );
    assert_eq!(
        server
            .request(
                "HEAD",
                &url.path_and_query()
                    .unwrap()
                    .as_str()
                    .replacen("%2D", "-", 1),
                "",
                ""
            )
            .status,
        403
    );
    assert_eq!(
        server.request("HEAD", "/_internal/ready", "", "").status,
        200
    );
}

#[test]
fn xml_service_returns_the_official_first_thousand_bucket_limit() {
    let data = TestDir::new();
    let file = data.0.join("buckets.json");
    let buckets: Vec<_> = (0..1000)
        .map(|n| json!({"name":format!("bucket-{n:04}")}))
        .collect();
    std::fs::write(
        &file,
        json!({"version":1,"project":"project-a","buckets":buckets}).to_string(),
    )
    .unwrap();
    let server = Server::start(
        &data,
        &[
            "--backend",
            "memory",
            "--buckets-file",
            file.to_str().unwrap(),
        ],
    );
    let reply = server.request("GET", "/", PROJECT, "");
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(names(&reply.body).len(), 1000);
    server.create("project-a", "bucket-1000");
    let reply = server.request("GET", "/", PROJECT, "");
    assert_eq!(reply.status, 200);
    assert_eq!(names(&reply.body).len(), 1000);
    assert_eq!(names(&reply.body).last(), Some(&"bucket-0999"));
    assert!(!reply.body.contains("bucket-1000"));
}

#[test]
fn xml_bucket_create_delete_defaults_fields_conflicts_nonempty_and_restart() {
    let data = TestDir::new();
    let server = Server::start(&data, &["--default-location", "EU"]);
    let created = server.request("PUT", "/xml%2Dbucket", PROJECT, "");
    assert_eq!(created.status, 200, "{}", created.body);
    assert!(created.body.is_empty());
    let resource = server.request("GET", "/storage/v1/b/xml-bucket", "", "");
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    assert_eq!(resource["location"], "US");
    assert_eq!(resource["storageClass"], "STANDARD");
    let duplicate = server.request("PUT", "/xml-bucket", PROJECT, "");
    assert_eq!(duplicate.status, 409);
    assert!(
        duplicate
            .body
            .contains("<Code>BucketNameUnavailable</Code>")
    );
    let body = "<?xml version=\"1.0\"?><CreateBucketConfiguration xmlns=\"http://doc.s3.amazonaws.com/2006-03-01\"><StorageClass>NEARLINE</StorageClass><LocationConstraint>asia-northeast1</LocationConstraint></CreateBucketConfiguration>";
    assert_eq!(
        server
            .request("PUT", "/regional-bucket/", PROJECT, body)
            .status,
        200
    );
    let resource = server.request("GET", "/storage/v1/b/regional-bucket", "", "");
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    assert_eq!(resource["location"], "ASIA-NORTHEAST1");
    assert_eq!(resource["storageClass"], "NEARLINE");
    assert_eq!(
        server
            .request("PUT", "/xml-bucket/value", "", "body")
            .status,
        200
    );
    let nonempty = server.request("DELETE", "/xml-bucket", "", "");
    assert_eq!(nonempty.status, 409);
    assert!(nonempty.body.contains("<Code>BucketNotEmpty</Code>"));
    assert_eq!(
        server.request("DELETE", "/xml-bucket/value", "", "").status,
        204
    );
    let deleted = server.request("DELETE", "/xml-bucket/", "", "");
    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    let missing = server.request("DELETE", "/xml-bucket", "", "");
    assert_eq!(missing.status, 404);
    assert!(missing.body.contains("<Code>NoSuchBucket</Code>"));
    drop(server);
    let server = Server::start(&data, &[]);
    assert_eq!(server.request("HEAD", "/xml-bucket", "", "").status, 404);
    assert_eq!(
        server.request("HEAD", "/regional-bucket", "", "").status,
        200
    );
}

#[test]
fn xml_bucket_create_rejects_invalid_bounded_metadata_without_creating() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    for body in [
        "<Wrong/>",
        "<?xml version=\"bad\"?><CreateBucketConfiguration/>",
        "<?xml garbage?><CreateBucketConfiguration/>",
        "<CreateBucketConfigurationxmlns=\"http://doc.s3.amazonaws.com/2006-03-01\"/>",
        "<?xml version=\"1.0\"?>",
        "<!DOCTYPE x><CreateBucketConfiguration/>",
        "<CreateBucketConfiguration><LocationConstraint>US</LocationConstraint>",
        "<CreateBucketConfiguration><StorageClass>STANDARD</StorageClass><StorageClass>ARCHIVE</StorageClass></CreateBucketConfiguration>",
        "<CreateBucketConfiguration><LocationConstraint>&external;</LocationConstraint></CreateBucketConfiguration>",
        "<CreateBucketConfiguration><LocationConstraint>mars</LocationConstraint></CreateBucketConfiguration>",
        "<CreateBucketConfiguration><StorageClass>INVALID</StorageClass></CreateBucketConfiguration>",
        "<CreateBucketConfiguration><CustomPlacementConfig><DataLocations/></CustomPlacementConfig></CreateBucketConfiguration>",
        "<CreateBucketConfiguration><EncryptionConfiguration/></CreateBucketConfiguration>",
        "<CreateBucketConfiguration>text</CreateBucketConfiguration>",
    ] {
        let reply = server.request("PUT", "/invalid-bucket", PROJECT, body);
        assert_eq!(reply.status, 400, "{body}: {}", reply.body);
        assert!(reply.body.contains("<Error>"));
        assert_eq!(
            server.request("HEAD", "/invalid-bucket", "", "").status,
            404
        );
    }
    for headers in [
        "",
        "x-goog-project-id: a\r\nx-goog-project-id: b\r\n",
        "x-goog-project-id: project-a\r\nx-goog-acl: public-read\r\n",
        "x-goog-project-id: project-a\r\nx-goog-bucket-object-lock-enabled: yes\r\n",
        "x-goog-project-id: project-a\r\nx-goog-bucket-retention-period: -1\r\n",
        "x-goog-project-id: project-a\r\nx-goog-bucket-retention-period: 315576000001\r\n",
    ] {
        assert_eq!(
            server.request("PUT", "/invalid-bucket", headers, "").status,
            400
        );
    }
    let reply = server.request("PUT", "/invalid-bucket", PROJECT, &"x".repeat(65537));
    assert_eq!(reply.status, 413);
    assert_eq!(
        server.request("HEAD", "/invalid-bucket", "", "").status,
        404
    );
    for path in ["/Invalid_Name", "/ab", "/192.168.1.1", "/google-bucket"] {
        assert_eq!(
            server.request("PUT", path, PROJECT, "").status,
            400,
            "{path}"
        );
    }
    for (method, path, headers, body) in [
        ("PUT", "/invalid-bucket?acl", PROJECT, ""),
        ("DELETE", "/invalid-bucket?generation=1", "", ""),
        ("DELETE", "/invalid-bucket", "", "body"),
    ] {
        assert_eq!(server.request(method, path, headers, body).status, 400);
    }
}

#[test]
fn xml_bucket_strict_creation_retention_signed_body_and_deletion_permissions() {
    let data = TestDir::new();
    let server = strict(&data);
    for (auth, status) in [("", 401), (NONE, 403), (VIEWER, 403)] {
        assert_eq!(
            server
                .request("PUT", "/strict-bucket", &format!("{auth}{PROJECT}"), "")
                .status,
            status
        );
    }
    let headers = format!(
        "{ADMIN}{PROJECT}x-goog-bucket-object-lock-enabled: true\r\nx-goog-bucket-retention-period: 60\r\n"
    );
    assert_eq!(
        server
            .host("strict-bucket.storage.local", "PUT", "/", &headers, "")
            .status,
        200
    );
    let resource = server.request("GET", "/storage/v1/b/strict-bucket", ADMIN, "");
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    assert_eq!(resource["objectRetention"]["mode"], "Enabled");
    assert_eq!(resource["retentionPolicy"]["retentionPeriod"], "60");
    assert_eq!(
        server
            .request("PUT", "/strict-bucket/protected", ADMIN, "retained")
            .status,
        200
    );
    let denied = server.request("DELETE", "/strict-bucket/protected", ADMIN, "");
    assert_eq!(denied.status, 403);
    assert!(denied.body.contains("RetentionPolicyNotMet"));
    assert_eq!(
        server
            .request("DELETE", "/strict-bucket", VIEWER, "")
            .status,
        403
    );
    assert_eq!(
        server.request("DELETE", "/strict-bucket", "", "").status,
        401
    );
    let body = "<CreateBucketConfiguration><StorageClass>ARCHIVE</StorageClass></CreateBucketConfiguration>";
    let sha = ring::digest::digest(&ring::digest::SHA256, body.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut signed_headers = hyper::HeaderMap::new();
    signed_headers.insert("x-goog-project-id", "project-a".parse().unwrap());
    signed_headers.insert("x-goog-content-sha256", sha.parse().unwrap());
    let url = server.sign(hyper::Method::PUT, "/signed-bucket", signed_headers);
    let headers = format!("{PROJECT}x-goog-content-sha256: {sha}\r\n");
    let mismatch = server.request(
        "PUT",
        url.path_and_query().unwrap().as_str(),
        &headers,
        "<CreateBucketConfiguration/>",
    );
    assert_eq!(mismatch.status, 400);
    assert!(mismatch.body.contains("BadDigest"));
    assert_eq!(
        server.request("HEAD", "/signed-bucket", ADMIN, "").status,
        404
    );
    assert_eq!(
        server
            .request(
                "PUT",
                url.path_and_query().unwrap().as_str(),
                &headers,
                body
            )
            .status,
        200
    );
    let url = server.sign(
        hyper::Method::DELETE,
        "/signed-bucket",
        hyper::HeaderMap::new(),
    );
    let deleted = server.request("DELETE", url.path_and_query().unwrap().as_str(), "", "");
    assert_eq!(deleted.status, 204, "{}", deleted.body);
}

#[test]
fn xml_delete_object_enforces_generation_metageneration_versioning_and_signed_binding() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "delete-bucket");
    let patch = server.request(
        "PATCH",
        "/storage/v1/b/delete-bucket",
        ADMIN,
        "{\"versioning\":{\"enabled\":true}}",
    );
    assert_eq!(patch.status, 200);
    assert_eq!(
        server
            .request("PUT", "/delete-bucket/a%2Fb", ADMIN, "first")
            .status,
        200
    );
    let first = server.request("GET", "/storage/v1/b/delete-bucket/o/a%2Fb", ADMIN, "");
    let first: Value = serde_json::from_str(&first.body).unwrap();
    let generation = first["generation"].as_str().unwrap();
    assert_eq!(
        server
            .request("PUT", "/delete-bucket/a%2Fb", ADMIN, "second")
            .status,
        200
    );
    for auth in [VIEWER, NONE, ""] {
        assert!(matches!(
            server
                .request("DELETE", "/delete-bucket/a%2Fb", auth, "")
                .status,
            401 | 403
        ));
    }
    for headers in [
        format!("{ADMIN}x-goog-if-generation-match: {generation}\r\n"),
        format!("{ADMIN}x-goog-if-metageneration-match: 2\r\n"),
    ] {
        let reply = server.request("DELETE", "/delete-bucket/a%2Fb", &headers, "");
        assert_eq!(reply.status, 412);
        assert!(reply.body.contains("PreconditionFailed"));
    }
    let url = server.sign(
        hyper::Method::DELETE,
        &format!("/delete-bucket/a%2Fb?generation={generation}"),
        hyper::HeaderMap::new(),
    );
    let path = url.path_and_query().unwrap().as_str();
    for changed in [
        path.replace("a%2Fb", "a/b"),
        path.replace(&format!("generation={generation}"), "generation=1"),
    ] {
        assert_eq!(server.request("DELETE", &changed, "", "").status, 403);
    }
    assert_eq!(
        server
            .host("changed.invalid", "DELETE", path, "", "")
            .status,
        403
    );
    assert_eq!(server.request("GET", path, "", "").status, 403);
    assert_eq!(server.request("DELETE", path, "", "").status, 204);
    assert_eq!(
        server
            .request(
                "GET",
                &format!("/delete-bucket/a%2Fb?generation={generation}"),
                ADMIN,
                ""
            )
            .status,
        404
    );
    assert_eq!(
        server
            .request("GET", "/delete-bucket/a%2Fb", ADMIN, "")
            .body,
        "second"
    );
    assert_eq!(
        server
            .request(
                "DELETE",
                "/delete-bucket/a%2Fb",
                &format!("{ADMIN}x-goog-if-metageneration-match: 1\r\n"),
                ""
            )
            .status,
        204
    );
    let missing = server.request("DELETE", "/delete-bucket/a%2Fb", ADMIN, "");
    assert_eq!(missing.status, 404);
    assert!(missing.body.contains("NoSuchKey"));
    assert_eq!(
        server.request("DELETE", "/delete-bucket", ADMIN, "").status,
        409
    );
    drop(server);
    let server = strict(&data);
    assert_eq!(
        server
            .request("GET", "/delete-bucket/a%2Fb", ADMIN, "")
            .status,
        404
    );
    let old = server.request(
        "GET",
        &format!("/delete-bucket/a%2Fb?generation={generation}"),
        ADMIN,
        "",
    );
    assert_eq!(old.status, 404);
}

#[test]
fn xml_bucket_create_accepts_standard_xml_declarations_namespaces_entities_and_cdata() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    for (index, body) in [
        "<?xml version = '1.0' encoding = \"UTF-8\" standalone = 'yes'?><CreateBucketConfiguration><LocationConstraint>U&#x53;</LocationConstraint><StorageClass>STANDARD</StorageClass></CreateBucketConfiguration>",
        "<?xml version=\"1.0\"?><s:CreateBucketConfiguration xmlns:s = 'http://doc.s3.amazonaws.com/2006-03-01'><s:LocationConstraint><![CDATA[US]]></s:LocationConstraint></s:CreateBucketConfiguration>",
        "<!--before--><CreateBucketConfiguration xmlns = \"http://doc.s3.amazonaws.com/2006-03-0&#49;\"><LocationConstraint>U<!--comment-->S</LocationConstraint></CreateBucketConfiguration><!--after-->",
        "<CreateBucketConfiguration xmlns='http://s3.amazonaws.com/doc/2006-03-01/' />",
    ].iter().enumerate() {
        let reply = server.request("PUT", &format!("/syntax-bucket-{index}"), PROJECT, body);
        assert_eq!(reply.status, 200, "{body}: {}", reply.body);
        let resource = server.request("GET", &format!("/storage/v1/b/syntax-bucket-{index}"), "", "");
        assert_eq!(serde_json::from_str::<Value>(&resource.body).unwrap()["location"], "US");
    }
    for body in [
        "<CreateBucketConfiguration/><CreateBucketConfiguration/>",
        "<CreateBucketConfiguration/>trailing",
        "<?xml version='1.0' version='1.0'?><CreateBucketConfiguration/>",
        "<?xml version='1.0' encoding='UTF-16'?><CreateBucketConfiguration/>",
        "<?xml version='1.0' standalone='maybe'?><CreateBucketConfiguration/>",
        "<CreateBucketConfiguration xmlns='wrong'/>",
        "<x:CreateBucketConfiguration/>",
        "<CreateBucketConfiguration><LocationConstraint xmlns='wrong'>US</LocationConstraint></CreateBucketConfiguration>",
        "<CreateBucketConfiguration xmlns='http://doc.s3.amazonaws.com/2006-03-01' xmlns='http://doc.s3.amazonaws.com/2006-03-01'/>",
        "<!DOCTYPE x [<!ENTITY e 'US'>]><CreateBucketConfiguration><LocationConstraint>&e;</LocationConstraint></CreateBucketConfiguration>",
    ] {
        let reply = server.request("PUT", "/syntax-invalid", PROJECT, body);
        assert_eq!(reply.status, 400, "{body}: {}", reply.body);
        assert!(reply.body.contains("MalformedXML"));
        assert_eq!(
            server.request("HEAD", "/syntax-invalid", "", "").status,
            404
        );
    }
}

#[test]
fn xml_bucket_create_verifies_md5_and_separates_retention_enable_permission() {
    let data = TestDir::new();
    let server = strict(&data);
    for (digest, status) in [
        ("AAAAAAAAAAAAAAAAAAAAAA==", 400),
        ("invalid", 400),
        ("1B2M2Y8AsgTpgAmY7PhCfg==", 200),
    ] {
        let headers = format!("{ADMIN}{PROJECT}Content-MD5: {digest}\r\n");
        let reply = server.request("PUT", "/md5-bucket", &headers, "");
        assert_eq!(reply.status, status, "{}", reply.body);
        assert_eq!(
            server.request("HEAD", "/md5-bucket", ADMIN, "").status,
            if status == 200 { 200 } else { 404 }
        );
    }
    let editor = "Authorization: Bearer editor-token\r\n";
    let headers = format!("{editor}{PROJECT}x-goog-bucket-object-lock-enabled: true\r\n");
    let reply = server.request("PUT", "/retention-denied", &headers, "");
    assert_eq!(reply.status, 403, "{}", reply.body);
    assert!(reply.body.contains("AccessDenied"));
    assert_eq!(
        server
            .request("HEAD", "/retention-denied", ADMIN, "")
            .status,
        404
    );
    let headers = format!("{editor}{PROJECT}x-goog-bucket-object-lock-enabled: false\r\n");
    assert_eq!(
        server
            .request("PUT", "/retention-denied", &headers, "")
            .status,
        200
    );
    assert_eq!(
        server
            .request("HEAD", "/retention-denied", ADMIN, "")
            .status,
        200
    );
}

#[test]
fn xml_head_bucket_uses_iam_ubla_and_public_access_prevention() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "policy-bucket");
    let policy = json!({"bindings":[{"role":"roles/storage.legacyBucketReader","members":["allUsers","user:no-roles@example.invalid"]}]}).to_string();
    assert_eq!(
        server
            .request("PUT", "/storage/v1/b/policy-bucket/iam", ADMIN, &policy)
            .status,
        200
    );
    for auth in ["", NONE] {
        assert_eq!(
            server.request("HEAD", "/policy-bucket", auth, "").status,
            200
        );
    }
    let patch = json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true},"publicAccessPrevention":"enforced"}}).to_string();
    assert_eq!(
        server
            .request("PATCH", "/storage/v1/b/policy-bucket", ADMIN, &patch)
            .status,
        200
    );
    assert_eq!(server.request("HEAD", "/policy-bucket", "", "").status, 401);
    assert_eq!(
        server.request("HEAD", "/policy-bucket", NONE, "").status,
        200
    );
    let url = server.sign(
        hyper::Method::HEAD,
        "/policy-bucket",
        hyper::HeaderMap::new(),
    );
    assert_eq!(
        server
            .request("HEAD", url.path_and_query().unwrap().as_str(), "", "")
            .status,
        200
    );
    assert_eq!(
        server
            .request("GET", "/", &format!("{NONE}{PROJECT}"), "")
            .status,
        403
    );
}

#[test]
fn xml_delete_object_preserves_soft_deleted_bytes_and_enforces_holds_and_pap() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "soft-bucket");
    let policy = json!({"softDeletePolicy":{"retentionDurationSeconds":"604800"}}).to_string();
    assert_eq!(
        server
            .request("PATCH", "/storage/v1/b/soft-bucket", ADMIN, &policy)
            .status,
        200
    );
    assert_eq!(
        server
            .request("PUT", "/soft-bucket/protected", ADMIN, "restore me")
            .status,
        200
    );
    let object = "/storage/v1/b/soft-bucket/o/protected";
    let resource = server.request("PATCH", object, ADMIN, "{\"temporaryHold\":true}");
    assert_eq!(resource.status, 200);
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    let generation = resource["generation"].as_str().unwrap();
    let denied = server.request("DELETE", "/soft-bucket/protected", ADMIN, "");
    assert_eq!(denied.status, 403);
    assert!(
        denied.body.contains("ObjectUnderActiveHold"),
        "{}",
        denied.body
    );
    assert_eq!(
        server
            .request("PATCH", object, ADMIN, "{\"temporaryHold\":false}")
            .status,
        200
    );
    let policy = json!({"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]})
        .to_string();
    assert_eq!(
        server
            .request("PUT", "/storage/v1/b/soft-bucket/iam", ADMIN, &policy)
            .status,
        200
    );
    let policy = json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true},"publicAccessPrevention":"enforced"}}).to_string();
    assert_eq!(
        server
            .request("PATCH", "/storage/v1/b/soft-bucket", ADMIN, &policy)
            .status,
        200
    );
    assert_eq!(
        server
            .request("DELETE", "/soft-bucket/protected", "", "")
            .status,
        401
    );
    let url = server.sign(
        hyper::Method::DELETE,
        &format!("/soft-bucket/protected?generation={generation}"),
        hyper::HeaderMap::new(),
    );
    assert_eq!(
        server
            .request("DELETE", url.path_and_query().unwrap().as_str(), "", "")
            .status,
        204
    );
    let soft = server.request(
        "GET",
        &format!("{object}?generation={generation}&softDeleted=true"),
        ADMIN,
        "",
    );
    assert_eq!(soft.status, 200, "{}", soft.body);
    assert!(serde_json::from_str::<Value>(&soft.body).unwrap()["softDeleteTime"].is_string());
    drop(server);
    let server = strict(&data);
    let restored = server.request(
        "POST",
        &format!("{object}/restore?generation={generation}"),
        ADMIN,
        "",
    );
    assert_eq!(restored.status, 200, "{}", restored.body);
    assert_eq!(
        server
            .request("GET", "/soft-bucket/protected", ADMIN, "")
            .body,
        "restore me"
    );
}
