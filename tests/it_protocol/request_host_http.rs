//! Links follow the authority each request addressed unless a base is configured explicitly.

use crate::common;
use crate::common::process::declare_server;
use common::TestDir;
use common::process::{Reply, http_command, request_head};
use serde_json::{Value, json};
use std::{io::Write, net::TcpStream, time::Duration};

declare_server!();

const CONTAINER: &str = "gcs:4443";
const PUBLISHED: &str = "localhost:54321";

impl Server {
    fn derived(dir: &TestDir, args: &[&str]) -> Self {
        Self::launch(http_command(&dir.0).args(args))
    }

    fn raw(&self, request: &[u8]) -> Reply {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream.write_all(request).unwrap();
        Reply::framed(stream)
    }

    fn send(&self, method: &str, path: &str, host: &str, headers: &str, body: &[u8]) -> Reply {
        let mut request = request_head(
            method,
            path,
            host,
            &format!("{headers}Connection: close\r\n"),
            body.len(),
        )
        .into_bytes();
        request.extend_from_slice(body);
        self.raw(&request)
    }

    fn bucket(&self, name: &str) {
        self.send(
            "POST",
            "/storage/v1/b?project=local",
            CONTAINER,
            "Content-Type: application/json\r\n",
            json!({"name": name}).to_string().as_bytes(),
        )
        .expect(200);
    }

    fn upload(&self, bucket: &str, name: &str, host: &str) -> Value {
        self.send(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}"),
            host,
            "",
            b"bytes",
        )
        .json_ok()
    }

    fn resumable_location(&self, bucket: &str, name: &str, host: &str) -> String {
        self.send(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=resumable&name={name}"),
            host,
            "",
            b"",
        )
        .expect(200)
        .headers["location"]
            .clone()
    }

    fn bind_base(&self) -> String {
        self.ready["api_base_url"].as_str().unwrap().to_owned()
    }
}

fn origin(host: &str) -> String {
    format!("http://{host}")
}

fn path_of<'a>(url: &'a str, base: &str) -> &'a str {
    url.strip_prefix(base)
        .unwrap_or_else(|| panic!("{url} does not start with {base}"))
}

#[track_caller]
fn assert_object_links(value: &Value, base: &str) {
    for key in ["selfLink", "mediaLink"] {
        let link = value[key]
            .as_str()
            .unwrap_or_else(|| panic!("{key}: {value}"));
        assert!(link.starts_with(&format!("{base}/")), "{key}: {link}");
    }
}

#[test]
fn every_rendered_link_follows_the_host_of_its_request() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("links");
    for host in [CONTAINER, PUBLISHED] {
        let base = origin(host);
        let get = |path: &str| server.send("GET", path, host, "", b"").json_ok();
        let location = server.resumable_location("links", "resumable", host);
        assert!(
            location.starts_with(&format!("{base}/upload/storage/v1/b/links/o?")),
            "{location}"
        );
        let object = server.upload("links", "object", host);
        assert_object_links(&object, &base);
        assert_object_links(&get("/storage/v1/b/links/o/object"), &base);
        let bucket = get("/storage/v1/b/links");
        assert_eq!(bucket["selfLink"], format!("{base}/storage/v1/b/links"));
        let listing = get("/storage/v1/b/links/o");
        assert_object_links(&listing["items"][0], &base);
        let buckets = get("/storage/v1/b?project=local");
        assert_eq!(
            buckets["items"][0]["selfLink"],
            format!("{base}/storage/v1/b/links")
        );
        let rewritten = server
            .send(
                "POST",
                "/storage/v1/b/links/o/object/rewriteTo/b/links/o/rewritten",
                host,
                "",
                b"",
            )
            .json_ok();
        assert_eq!(rewritten["done"], true);
        assert_object_links(&rewritten["resource"], &base);
        let copied = server
            .send(
                "POST",
                "/storage/v1/b/links/o/object/copyTo/b/links/o/copied",
                host,
                "",
                b"",
            )
            .json_ok();
        assert_object_links(&copied, &base);
        let patched = server
            .send(
                "PATCH",
                "/storage/v1/b/links/o/object",
                host,
                "Content-Type: application/json\r\n",
                br#"{"metadata":{"k":"v"}}"#,
            )
            .json_ok();
        assert_object_links(&patched, &base);
    }
}

#[test]
fn a_session_opened_through_one_host_completes_through_another() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("sessions");
    let location = server.resumable_location("sessions", "crossed", CONTAINER);
    let container = origin(CONTAINER);
    let session = path_of(&location, &container);
    let finish = |host: &str| {
        server
            .send(
                "PUT",
                session,
                host,
                "Content-Range: bytes 0-4/5\r\n",
                b"bytes",
            )
            .json_ok()
    };
    let completed = finish(PUBLISHED);
    assert_object_links(&completed, &origin(PUBLISHED));
    // The session keeps the completed response; its links still name the retry's own address.
    let retried = finish(CONTAINER);
    assert_object_links(&retried, &origin(CONTAINER));
    assert_eq!(completed["generation"], retried["generation"]);
    assert_eq!(completed["md5Hash"], retried["md5Hash"]);
    assert_object_links(&finish(PUBLISHED), &origin(PUBLISHED));
}

#[test]
fn replayed_uploads_render_links_for_the_retrying_host() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("replay");
    let upload = |host: &str| {
        server
            .send(
                "POST",
                "/upload/storage/v1/b/replay/o?uploadType=media&name=token",
                host,
                "X-Goog-Gcs-Idempotency-Token: retry-token\r\n",
                b"bytes",
            )
            .json_ok()
    };
    let first = upload(CONTAINER);
    let second = upload(PUBLISHED);
    assert_eq!(first["generation"], second["generation"]);
    assert_object_links(&first, &origin(CONTAINER));
    assert_object_links(&second, &origin(PUBLISHED));
}

#[test]
fn a_repeated_call_on_a_finished_rewrite_renders_links_for_its_own_host() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("rewrite");
    let source = vec![b'a'; 1024 * 1024 + 7];
    server
        .send(
            "POST",
            "/upload/storage/v1/b/rewrite/o?uploadType=media&name=source",
            CONTAINER,
            "",
            &source,
        )
        .json_ok();
    let rewrite = "/storage/v1/b/rewrite/o/source/rewriteTo/b/rewrite/o/destination";
    let first = server
        .send(
            "POST",
            &format!("{rewrite}?maxBytesRewrittenPerCall=1048576"),
            CONTAINER,
            "",
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json_ok();
    assert_eq!(first["done"], false);
    let token = first["rewriteToken"].as_str().unwrap();
    let continued = format!("{rewrite}?rewriteToken={token}");
    let finished = server
        .send("POST", &continued, CONTAINER, "", b"")
        .json_ok();
    assert_eq!(finished["done"], true);
    assert_object_links(&finished["resource"], &origin(CONTAINER));
    let repeated = server
        .send("POST", &continued, PUBLISHED, "", b"")
        .json_ok();
    assert_eq!(repeated["done"], true);
    assert_object_links(&repeated["resource"], &origin(PUBLISHED));
}

#[test]
fn a_metadata_write_may_repeat_links_read_through_another_host() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("repeat");
    let object = server.upload("repeat", "object", PUBLISHED);
    let body = json!({
        "selfLink": object["selfLink"],
        "mediaLink": object["mediaLink"],
        "metadata": {"k": "v"},
    })
    .to_string();
    let patched = server
        .send(
            "PATCH",
            "/storage/v1/b/repeat/o/object",
            CONTAINER,
            "Content-Type: application/json\r\n",
            body.as_bytes(),
        )
        .json_ok();
    assert_object_links(&patched, &origin(CONTAINER));
}

#[test]
fn xml_session_and_multipart_locations_follow_the_host_of_their_request() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("xml-links");
    for host in [CONTAINER, PUBLISHED] {
        let base = origin(host);
        let location = server
            .send(
                "POST",
                "/xml-links/resumable",
                host,
                "x-goog-resumable: start\r\n",
                b"",
            )
            .expect(201)
            .headers["location"]
            .clone();
        assert!(
            location.starts_with(&format!("{base}/xml-links/resumable?upload_id=")),
            "{location}"
        );
        let started = server
            .send("POST", "/xml-links/multipart?uploads", host, "", b"")
            .expect(200)
            .body
            .clone();
        let started = String::from_utf8(started).unwrap();
        let upload_id = started
            .split_once("<UploadId>")
            .and_then(|(_, rest)| rest.split_once("</UploadId>"))
            .unwrap()
            .0;
        let part = server
            .send(
                "PUT",
                &format!("/xml-links/multipart?partNumber=1&uploadId={upload_id}"),
                host,
                "",
                b"part",
            )
            .expect(200)
            .headers["etag"]
            .clone();
        let manifest = format!(
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{part}</ETag></Part></CompleteMultipartUpload>"
        );
        let completed = server
            .send(
                "POST",
                &format!("/xml-links/multipart?uploadId={upload_id}"),
                host,
                "",
                manifest.as_bytes(),
            )
            .expect(200)
            .body
            .clone();
        let completed = String::from_utf8(completed).unwrap();
        assert!(
            completed.contains(&format!("<Location>{base}/xml-links/multipart</Location>")),
            "{completed}"
        );
    }
}

#[test]
fn batch_parts_use_the_envelope_host_and_only_that_origin() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("batched");
    server.upload("batched", "object", CONTAINER);
    let envelope = |target: &str| {
        format!(
            "--batch\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\nContent-ID: 1\r\n\r\nGET {target} HTTP/1.1\r\n\r\n\r\n--batch--\r\n"
        )
    };
    let send = |host: &str, target: &str| {
        server.send(
            "POST",
            "/batch/storage/v1",
            host,
            "Content-Type: multipart/mixed; boundary=batch\r\n",
            envelope(target).as_bytes(),
        )
    };
    for target in [
        "/storage/v1/b/batched/o/object".to_owned(),
        format!("{}/storage/v1/b/batched/o/object", origin(PUBLISHED)),
    ] {
        let reply = send(PUBLISHED, &target).expect(200).body.clone();
        let reply = String::from_utf8(reply).unwrap();
        assert!(
            reply.contains(&format!(
                "\"{}/storage/v1/b/batched/o/object\"",
                origin(PUBLISHED)
            )),
            "{reply}"
        );
    }
    // A part addressing another origin is refused as before, whatever the envelope's Host is.
    send(
        PUBLISHED,
        &format!("{}/storage/v1/b/batched/o/object", origin(CONTAINER)),
    )
    .expect(400);
}

#[test]
fn forwarded_headers_and_unusable_hosts_never_reach_an_advertised_link() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &[]);
    server.bucket("hostile");
    let forwarded = "X-Forwarded-Host: evil.invalid\r\nX-Forwarded-Proto: https\r\nForwarded: host=evil.invalid;proto=https\r\n";
    let path = "/upload/storage/v1/b/hostile/o?uploadType=resumable&name=hostile";
    let location = server
        .send("POST", path, CONTAINER, forwarded, b"")
        .expect(200)
        .headers["location"]
        .clone();
    assert!(
        location.starts_with(&format!("{}/", origin(CONTAINER))),
        "{location}"
    );

    let bind = server.bind_base();
    for host in [
        "",
        "a@b",
        "user:secret@gcs:4443",
        "gcs/path",
        "gcs:4443/path",
        "gcs\tinjected",
        "gcs:",
        "gcs:0",
        "gcs:99999",
        "[::1",
        "[zz]",
    ] {
        let reply = server.send("POST", path, host, "", b"");
        reply.expect_with(200, host);
        let location = &reply.headers["location"];
        assert!(
            location.starts_with(&format!("{bind}/")),
            "{host:?}: {location}"
        );
    }
    let no_host = server.raw(
        format!("POST {path} HTTP/1.0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    );
    no_host.expect(200);
    assert!(
        no_host.headers["location"].starts_with(&format!("{bind}/")),
        "{:?}",
        no_host.headers
    );
    let repeated = server.raw(
        format!(
            "POST {path} HTTP/1.1\r\nHost: gcs:4443\r\nHost: localhost:54321\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    );
    repeated.expect(200);
    assert!(
        repeated.headers["location"].starts_with(&format!("{bind}/")),
        "{:?}",
        repeated.headers
    );
    let absolute = server.raw(
        format!(
            "POST http://gcs:4443{path} HTTP/1.1\r\nHost: localhost:54321\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    );
    absolute.expect(200);
    assert!(
        absolute.headers["location"].starts_with(&format!("{}/", origin(CONTAINER))),
        "{:?}",
        absolute.headers
    );
    let ipv6 = server.send("POST", path, "[::1]:4443", "", b"");
    ipv6.expect(200);
    assert!(
        ipv6.headers["location"].starts_with("http://[::1]:4443/"),
        "{:?}",
        ipv6.headers
    );
}

/// A peer container reaches the server through the virtual host's name, so a link built from the
/// bound listener would point it at its own loopback. The bucket label stays out of the base:
/// that host names one bucket, and the session path repeats the bucket name.
#[test]
fn an_xml_virtual_host_advertises_its_configured_domain_as_the_api_base() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &["--xml-virtual-host-domains", "storage.invalid"]);
    server.bucket("virtual");
    let reply = server.send(
        "POST",
        "/resumable",
        "virtual.storage.invalid:4443",
        "x-goog-resumable: start\r\n",
        b"",
    );
    reply.expect(201);
    let location = &reply.headers["location"];
    let domain_base = "http://storage.invalid:4443";
    assert!(
        location.starts_with(&format!("{domain_base}/virtual/resumable?upload_id=")),
        "{:?}",
        reply.headers
    );
    // Following the link addresses the same session path-style.
    server
        .send(
            "PUT",
            path_of(location, domain_base),
            "storage.invalid:4443",
            "Content-Range: bytes 0-4/5\r\n",
            b"bytes",
        )
        .expect(200);
    server
        .send(
            "GET",
            "/storage/v1/b/virtual/o/resumable",
            CONTAINER,
            "",
            b"",
        )
        .expect(200);
    let base_domain = server.send(
        "POST",
        "/upload/storage/v1/b/virtual/o?uploadType=resumable&name=named",
        "storage.invalid:4443",
        "",
        b"",
    );
    base_domain.expect(200);
    assert!(
        base_domain.headers["location"].starts_with("http://storage.invalid:4443/"),
        "{:?}",
        base_domain.headers
    );
}

#[test]
fn an_explicit_base_stays_fixed_behind_an_xml_virtual_host() {
    let dir = TestDir::new();
    let explicit = "https://advertised.invalid/api";
    let server = Server::derived(
        &dir,
        &[
            "--api-base-url",
            explicit,
            "--xml-virtual-host-domains",
            "storage.invalid",
        ],
    );
    server.bucket("virtual");
    let reply = server.send(
        "POST",
        "/resumable",
        "virtual.storage.invalid:4443",
        "x-goog-resumable: start\r\n",
        b"",
    );
    reply.expect(201);
    assert!(
        reply.headers["location"].starts_with(&format!("{explicit}/virtual/resumable?upload_id=")),
        "{:?}",
        reply.headers
    );
}

#[test]
fn an_explicit_base_ignores_the_host() {
    let dir = TestDir::new();
    let explicit = "https://advertised.invalid/api";
    let server = Server::derived(&dir, &["--api-base-url", explicit]);
    server.bucket("explicit");
    for host in [CONTAINER, PUBLISHED] {
        let location = server.resumable_location("explicit", "named", host);
        assert!(location.starts_with(&format!("{explicit}/")), "{location}");
        assert_object_links(&server.upload("explicit", "object", host), explicit);
    }
    // The completed response is stored as rendered, so the explicit base still names it.
    let location = server.resumable_location("explicit", "retried", CONTAINER);
    let session = path_of(&location, explicit);
    for host in [CONTAINER, PUBLISHED] {
        let completed = server
            .send(
                "PUT",
                session,
                host,
                "Content-Range: bytes 0-4/5\r\n",
                b"bytes",
            )
            .json_ok();
        assert_object_links(&completed, explicit);
    }
}

#[test]
fn runtime_external_url_fixes_the_base_and_a_public_host_alone_does_not() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &["--admin-enabled", "true"]);
    server.bucket("runtime");
    let configure = |body: Value| {
        server
            .send(
                "PUT",
                "/_internal/config",
                CONTAINER,
                "",
                body.to_string().as_bytes(),
            )
            .expect(200);
    };
    configure(json!({"publicHost": "public.invalid:9876"}));
    assert_object_links(
        &server.upload("runtime", "after-public", PUBLISHED),
        &origin(PUBLISHED),
    );
    configure(json!({"externalUrl": "http://fixed.invalid:9876"}));
    for host in [CONTAINER, PUBLISHED] {
        assert_object_links(
            &server.upload("runtime", "after-external", host),
            "http://fixed.invalid:9876",
        );
    }
}

#[test]
fn management_routes_refuse_a_host_that_only_the_derived_base_would_name() {
    let dir = TestDir::new();
    let server = Server::derived(&dir, &["--admin-enabled", "true"]);
    let evil = "evil.example.com";
    // The API itself still answers and advertises that name.
    server.bucket("guarded");
    assert_object_links(&server.upload("guarded", "object", evil), &origin(evil));
    for (method, path, body) in [
        ("PUT", "/_internal/config", b"{}".as_slice()),
        ("POST", "/_internal/reset", br#"{"confirm":"gcs-mimic"}"#),
        ("POST", "/_internal/delete_all", b""),
        ("POST", "/_internal/reseed", b""),
    ] {
        assert_eq!(
            server.send(method, path, evil, "", body).status,
            421,
            "{path}"
        );
    }
    server
        .send("GET", "/storage/v1/b/guarded/o/object", CONTAINER, "", b"")
        .expect(200);
    // A configured name stays admitted, and only through its explicit configuration.
    server
        .send(
            "PUT",
            "/_internal/config",
            CONTAINER,
            "",
            json!({"externalUrl": "http://api.example.com"})
                .to_string()
                .as_bytes(),
        )
        .expect(200);
    server
        .send("PUT", "/_internal/config", "api.example.com", "", b"{}")
        .expect(200);
    assert_eq!(
        server
            .send("PUT", "/_internal/config", evil, "", b"{}")
            .status,
        421
    );
}
