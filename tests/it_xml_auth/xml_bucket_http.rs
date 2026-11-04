use crate::common;
use crate::common::process::declare_server;

use common::TestDir;
use common::process::http_command;
use gcs_mimic::auth::signature::{SignUrlRequest, sign_url};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};

const ADMIN: &str = "Authorization: Bearer admin-token\r\n";
const VIEWER: &str = "Authorization: Bearer viewer-token\r\n";
const NONE: &str = "Authorization: Bearer no-roles-token\r\n";
const PROJECT: &str = "x-goog-project-id: project-a\r\n";
const PRIVATE: &[u8] = include_bytes!("../../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../../fixtures/signing/test-only-public.pem");

use common::http::{Client, TextReply as Reply};

impl Server {
    fn start(data: &TestDir, options: &[&str]) -> Self {
        Self::launch(
            http_command(&data.0.join("data"))
                .args(["--connection-timeout-ms", "20000"])
                .args(options),
        )
        .graceful()
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &str) -> Reply {
        self.host("localhost", method, path, headers, body)
    }

    fn host(&self, host: &str, method: &str, path: &str, headers: &str, body: &str) -> Reply {
        let mut reply = Client {
            address: &self.address,
            host,
            headers: "",
            timeout: Duration::from_secs(30),
        }
        .request_eof(method, path, headers, body.as_bytes())
        .into_text();
        reply.headers.make_ascii_lowercase();
        reply
    }

    fn create(&self, project: &str, name: &str) {
        let reply = self.request(
            "POST",
            &format!("/storage/v1/b?project={project}"),
            &format!("{ADMIN}Content-Type: application/json\r\n"),
            &json!({"name": name}).to_string(),
        );
        reply.expect(200);
    }

    fn sign(&self, method: hyper::Method, path: &str, headers: hyper::HeaderMap) -> hyper::Uri {
        sign_url(SignUrlRequest {
            url: &format!("http://localhost{path}"),
            method: &method,
            signer: "admin@example.invalid",
            private_key: PRIVATE,
            expires: 60,
            now: SystemTime::now(),
            headers,
        })
        .unwrap()
        .parse()
        .unwrap()
    }
}

fn strict(data: &TestDir) -> Server {
    let identities = common::process::write_identities(
        &data.0,
        &json!({"identities":[
            {"id":"admin@example.invalid", "tokens":["admin-token"], "public_keys":[PUBLIC],
             "project_roles":{"project-a":["roles/storage.admin"],"project-b":["roles/storage.admin"]}},
            {"id":"viewer@example.invalid", "tokens":["viewer-token"],
             "project_roles":{"project-a":["roles/storage.objectViewer"]}},
            {"id":"no-roles@example.invalid", "tokens":["no-roles-token"]},
            {"id":"editor@example.invalid", "tokens":["editor-token"],
             "project_roles":{"project-a":["roles/storage.editor"]}}
        ]}),
    );
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
fn xml_bucket_requests_reject_object_preconditions() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "condition-bucket");
    for (method, header, message) in [
        (
            "GET",
            "x-goog-if-generation-match: 0\r\n",
            "object conditions or encryption on bucket listing",
        ),
        (
            "DELETE",
            "x-goog-if-metageneration-match: 1\r\n",
            "object conditions, ranges or encryption on a bucket",
        ),
    ] {
        let reply = server.request(method, "/condition-bucket", &format!("{ADMIN}{header}"), "");
        reply.expect(400);
        assert!(reply.body.contains(message), "{}", reply.body);
        for (auth, status) in [("", 401), (NONE, 403)] {
            let reply = server.request(method, "/condition-bucket", &format!("{auth}{header}"), "");
            assert_eq!(reply.status, status, "{}", reply.body);
            assert!(!reply.body.contains(message), "{}", reply.body);
        }
    }
}

#[test]
fn xml_service_lists_only_selected_project_in_order_with_persisted_creation_dates() {
    let data = TestDir::new();
    let server = Server::start(&data, &[]);
    server.create("project-a", "zulu-bucket");
    server.create("project-b", "hidden-bucket");
    server.create("project-a", "alpha-bucket");
    let reply = server.request("GET", "/", PROJECT, "");
    reply.expect(200);
    assert!(reply.headers.contains("content-type: application/xml"));
    assert!(
        reply
            .body
            .contains("<ListAllMyBucketsResult xmlns='http://doc.s3.amazonaws.com/2006-03-01'>")
    );
    assert_eq!(names(&reply.body), ["alpha-bucket", "zulu-bucket"]);
    let metadata = server.request("GET", "/storage/v1/b/alpha-bucket", "", "");
    let metadata: Value = serde_json::from_str(&metadata.body).unwrap();
    assert!(reply.body.contains(&format!(
        "<CreationDate>{}</CreationDate>",
        metadata["timeCreated"].as_str().unwrap()
    )));
    let empty = server.request("GET", "/", "x-goog-project-id: empty\r\n", "");
    empty.expect(200);
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
        reply.expect_with(400, format_args!("{path} {headers}: {}", reply.body));
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
    server.request("GET", path, PROJECT, "").expect(200);
    let changed = server.request("GET", path, "x-goog-project-id: project-b\r\n", "");
    changed.expect(403);
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
    reply.expect(200);
    assert!(reply.body.is_empty());
    let url = server.sign(
        hyper::Method::HEAD,
        "/head%2Dbucket",
        hyper::HeaderMap::new(),
    );
    server
        .request("HEAD", url.path_and_query().unwrap().as_str(), "", "")
        .expect(200);
    server
        .request(
            "HEAD",
            &url.path_and_query()
                .unwrap()
                .as_str()
                .replacen("%2D", "-", 1),
            "",
            "",
        )
        .expect(403);
    server
        .request("HEAD", "/_internal/ready", "", "")
        .expect(200);
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
    reply.expect(200);
    assert_eq!(names(&reply.body).len(), 1000);
    server.create("project-a", "bucket-1000");
    let reply = server.request("GET", "/", PROJECT, "");
    reply.expect(200);
    assert_eq!(names(&reply.body).len(), 1000);
    assert_eq!(names(&reply.body).last(), Some(&"bucket-0999"));
    assert!(!reply.body.contains("bucket-1000"));
}

#[test]
fn xml_bucket_create_delete_defaults_fields_conflicts_nonempty_and_restart() {
    let data = TestDir::new();
    let server = Server::start(&data, &["--default-location", "EU"]);
    let created = server.request("PUT", "/xml%2Dbucket", PROJECT, "");
    created.expect(200);
    assert!(created.body.is_empty());
    let resource = server.request("GET", "/storage/v1/b/xml-bucket", "", "");
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    assert_eq!(resource["location"], "EU");
    assert_eq!(resource["storageClass"], "STANDARD");
    let duplicate = server.request("PUT", "/xml-bucket", PROJECT, "");
    duplicate.expect(409);
    assert!(
        duplicate
            .body
            .contains("<Code>BucketNameUnavailable</Code>")
    );
    let body = "<?xml version=\"1.0\"?><CreateBucketConfiguration xmlns=\"http://doc.s3.amazonaws.com/2006-03-01\"><StorageClass>NEARLINE</StorageClass><LocationConstraint>asia-northeast1</LocationConstraint></CreateBucketConfiguration>";
    server
        .request("PUT", "/regional-bucket/", PROJECT, body)
        .expect(200);
    let resource = server.request("GET", "/storage/v1/b/regional-bucket", "", "");
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    assert_eq!(resource["location"], "ASIA-NORTHEAST1");
    assert_eq!(resource["storageClass"], "NEARLINE");
    server
        .request("PUT", "/xml-bucket/value", "", "body")
        .expect(200);
    let nonempty = server.request("DELETE", "/xml-bucket", "", "");
    nonempty.expect(409);
    assert!(nonempty.body.contains("<Code>BucketNotEmpty</Code>"));
    server
        .request("DELETE", "/xml-bucket/value", "", "")
        .expect(204);
    let deleted = server.request("DELETE", "/xml-bucket/", "", "");
    deleted.expect(204);
    assert!(deleted.body.is_empty());
    let missing = server.request("DELETE", "/xml-bucket", "", "");
    missing.expect(404);
    assert!(missing.body.contains("<Code>NoSuchBucket</Code>"));
    drop(server);
    let server = Server::start(&data, &[]);
    server.request("HEAD", "/xml-bucket", "", "").expect(404);
    server
        .request("HEAD", "/regional-bucket", "", "")
        .expect(200);
}

#[test]
fn xml_bucket_creation_uses_configured_default_and_preserves_explicit_location() {
    let data = TestDir::new();
    let server = Server::start(&data, &["--default-location", "europe-west1"]);
    for (name, body, expected) in [
        ("empty-default", "", "EUROPE-WEST1"),
        (
            "xml-default",
            "<CreateBucketConfiguration><StorageClass>NEARLINE</StorageClass></CreateBucketConfiguration>",
            "EUROPE-WEST1",
        ),
        (
            "explicit-location",
            "<CreateBucketConfiguration><LocationConstraint>US</LocationConstraint></CreateBucketConfiguration>",
            "US",
        ),
    ] {
        server
            .request("PUT", &format!("/{name}"), PROJECT, body)
            .expect(200);
        let reply = server.request("GET", &format!("/storage/v1/b/{name}"), "", "");
        reply.expect(200);
        let resource: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(resource["location"], expected, "{name}");
    }
    let invalid = "<CreateBucketConfiguration><LocationConstraint>invalid</LocationConstraint></CreateBucketConfiguration>";
    server
        .request("PUT", "/invalid-location", PROJECT, invalid)
        .expect(400);
    server
        .request("HEAD", "/invalid-location", "", "")
        .expect(404);
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
        reply.expect_with(400, format_args!("{body}: {}", reply.body));
        assert!(reply.body.contains("<Error>"));
        server
            .request("HEAD", "/invalid-bucket", "", "")
            .expect(404);
    }
    for headers in [
        "",
        "x-goog-project-id: a\r\nx-goog-project-id: b\r\n",
        "x-goog-project-id: project-a\r\nx-goog-acl: public-read\r\n",
        "x-goog-project-id: project-a\r\nx-goog-bucket-object-lock-enabled: yes\r\n",
        "x-goog-project-id: project-a\r\nx-goog-bucket-retention-period: -1\r\n",
        "x-goog-project-id: project-a\r\nx-goog-bucket-retention-period: 315576000001\r\n",
    ] {
        server
            .request("PUT", "/invalid-bucket", headers, "")
            .expect(400);
    }
    let reply = server.request("PUT", "/invalid-bucket", PROJECT, &"x".repeat(65537));
    reply.expect(413);
    server
        .request("HEAD", "/invalid-bucket", "", "")
        .expect(404);
    for path in ["/Invalid_Name", "/ab", "/192.168.1.1", "/google-bucket"] {
        server
            .request("PUT", path, PROJECT, "")
            .expect_with(400, format_args!("{path}"));
    }
    for (method, path, headers, body) in [
        ("PUT", "/invalid-bucket?acl", PROJECT, ""),
        ("DELETE", "/invalid-bucket?generation=1", "", ""),
        ("DELETE", "/invalid-bucket", "", "body"),
    ] {
        server.request(method, path, headers, body).expect(400);
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
    server
        .host("strict-bucket.storage.local", "PUT", "/", &headers, "")
        .expect(200);
    let resource = server.request("GET", "/storage/v1/b/strict-bucket", ADMIN, "");
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    assert_eq!(resource["objectRetention"]["mode"], "Enabled");
    assert_eq!(resource["retentionPolicy"]["retentionPeriod"], "60");
    server
        .request("PUT", "/strict-bucket/protected", ADMIN, "retained")
        .expect(200);
    let denied = server.request("DELETE", "/strict-bucket/protected", ADMIN, "");
    denied.expect(403);
    assert!(denied.body.contains("RetentionPolicyNotMet"));
    server
        .request("DELETE", "/strict-bucket", VIEWER, "")
        .expect(403);
    server
        .request("DELETE", "/strict-bucket", "", "")
        .expect(401);
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
    mismatch.expect(400);
    assert!(mismatch.body.contains("BadDigest"));
    server
        .request("HEAD", "/signed-bucket", ADMIN, "")
        .expect(404);
    server
        .request(
            "PUT",
            url.path_and_query().unwrap().as_str(),
            &headers,
            body,
        )
        .expect(200);
    let url = server.sign(
        hyper::Method::DELETE,
        "/signed-bucket",
        hyper::HeaderMap::new(),
    );
    let deleted = server.request("DELETE", url.path_and_query().unwrap().as_str(), "", "");
    deleted.expect(204);
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
    patch.expect(200);
    server
        .request("PUT", "/delete-bucket/a%2Fb", ADMIN, "first")
        .expect(200);
    let first = server.request("GET", "/storage/v1/b/delete-bucket/o/a%2Fb", ADMIN, "");
    let first: Value = serde_json::from_str(&first.body).unwrap();
    let generation = first["generation"].as_str().unwrap();
    server
        .request("PUT", "/delete-bucket/a%2Fb", ADMIN, "second")
        .expect(200);
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
        reply.expect(412);
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
        server.request("DELETE", &changed, "", "").expect(403);
    }
    server
        .host("changed.invalid", "DELETE", path, "", "")
        .expect(403);
    server.request("GET", path, "", "").expect(403);
    server.request("DELETE", path, "", "").expect(204);
    server
        .request(
            "GET",
            &format!("/delete-bucket/a%2Fb?generation={generation}"),
            ADMIN,
            "",
        )
        .expect(404);
    assert_eq!(
        server
            .request("GET", "/delete-bucket/a%2Fb", ADMIN, "")
            .body,
        "second"
    );
    server
        .request(
            "DELETE",
            "/delete-bucket/a%2Fb",
            &format!("{ADMIN}x-goog-if-metageneration-match: 1\r\n"),
            "",
        )
        .expect(204);
    let missing = server.request("DELETE", "/delete-bucket/a%2Fb", ADMIN, "");
    missing.expect(404);
    assert!(missing.body.contains("NoSuchKey"));
    server
        .request("DELETE", "/delete-bucket", ADMIN, "")
        .expect(409);
    drop(server);
    let server = strict(&data);
    server
        .request("GET", "/delete-bucket/a%2Fb", ADMIN, "")
        .expect(404);
    let old = server.request(
        "GET",
        &format!("/delete-bucket/a%2Fb?generation={generation}"),
        ADMIN,
        "",
    );
    old.expect(404);
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
        reply.expect_with(200, format_args!("{body}: {}", reply.body));
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
        reply.expect_with(400, format_args!("{body}: {}", reply.body));
        assert!(reply.body.contains("MalformedXML"));
        server
            .request("HEAD", "/syntax-invalid", "", "")
            .expect(404);
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
    reply.expect(403);
    assert!(reply.body.contains("AccessDenied"));
    server
        .request("HEAD", "/retention-denied", ADMIN, "")
        .expect(404);
    let headers = format!("{editor}{PROJECT}x-goog-bucket-object-lock-enabled: false\r\n");
    server
        .request("PUT", "/retention-denied", &headers, "")
        .expect(200);
    server
        .request("HEAD", "/retention-denied", ADMIN, "")
        .expect(200);
}

#[test]
fn xml_head_bucket_uses_iam_ubla_and_public_access_prevention() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "policy-bucket");
    let policy = json!({"bindings":[{"role":"roles/storage.legacyBucketReader","members":["allUsers","user:no-roles@example.invalid"]}]}).to_string();
    server
        .request("PUT", "/storage/v1/b/policy-bucket/iam", ADMIN, &policy)
        .expect(200);
    for auth in ["", NONE] {
        server
            .request("HEAD", "/policy-bucket", auth, "")
            .expect(200);
    }
    let patch = json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true},"publicAccessPrevention":"enforced"}}).to_string();
    server
        .request("PATCH", "/storage/v1/b/policy-bucket", ADMIN, &patch)
        .expect(200);
    server.request("HEAD", "/policy-bucket", "", "").expect(401);
    server
        .request("HEAD", "/policy-bucket", NONE, "")
        .expect(200);
    let url = server.sign(
        hyper::Method::HEAD,
        "/policy-bucket",
        hyper::HeaderMap::new(),
    );
    server
        .request("HEAD", url.path_and_query().unwrap().as_str(), "", "")
        .expect(200);
    server
        .request("GET", "/", &format!("{NONE}{PROJECT}"), "")
        .expect(403);
}

#[test]
fn xml_delete_object_preserves_soft_deleted_bytes_and_enforces_holds_and_pap() {
    let data = TestDir::new();
    let server = strict(&data);
    server.create("project-a", "soft-bucket");
    let policy = json!({"softDeletePolicy":{"retentionDurationSeconds":"604800"}}).to_string();
    server
        .request("PATCH", "/storage/v1/b/soft-bucket", ADMIN, &policy)
        .expect(200);
    server
        .request("PUT", "/soft-bucket/protected", ADMIN, "restore me")
        .expect(200);
    let object = "/storage/v1/b/soft-bucket/o/protected";
    let resource = server.request("PATCH", object, ADMIN, "{\"temporaryHold\":true}");
    resource.expect(200);
    let resource: Value = serde_json::from_str(&resource.body).unwrap();
    let generation = resource["generation"].as_str().unwrap();
    let denied = server.request("DELETE", "/soft-bucket/protected", ADMIN, "");
    denied.expect(403);
    assert!(
        denied.body.contains("ObjectUnderActiveHold"),
        "{}",
        denied.body
    );
    server
        .request("PATCH", object, ADMIN, "{\"temporaryHold\":false}")
        .expect(200);
    let policy = json!({"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]})
        .to_string();
    server
        .request("PUT", "/storage/v1/b/soft-bucket/iam", ADMIN, &policy)
        .expect(200);
    let policy = json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true},"publicAccessPrevention":"enforced"}}).to_string();
    server
        .request("PATCH", "/storage/v1/b/soft-bucket", ADMIN, &policy)
        .expect(200);
    server
        .request("DELETE", "/soft-bucket/protected", "", "")
        .expect(401);
    let url = server.sign(
        hyper::Method::DELETE,
        &format!("/soft-bucket/protected?generation={generation}"),
        hyper::HeaderMap::new(),
    );
    server
        .request("DELETE", url.path_and_query().unwrap().as_str(), "", "")
        .expect(204);
    let soft = server.request(
        "GET",
        &format!("{object}?generation={generation}&softDeleted=true"),
        ADMIN,
        "",
    );
    soft.expect(200);
    assert!(serde_json::from_str::<Value>(&soft.body).unwrap()["softDeleteTime"].is_string());
    drop(server);
    let server = strict(&data);
    let restored = server.request(
        "POST",
        &format!("{object}/restore?generation={generation}"),
        ADMIN,
        "",
    );
    restored.expect(200);
    assert_eq!(
        server
            .request("GET", "/soft-bucket/protected", ADMIN, "")
            .body,
        "restore me"
    );
}

declare_server!(graceful);
