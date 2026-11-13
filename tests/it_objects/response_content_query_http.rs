//! `response-content-*` overrides on JSON object reads, which Cloud Storage accepted and ignored
//! on 2026-11-09.

use crate::common;
use common::TestDir;
use common::effects::Server;

const OBJECT: &str = "/storage/v1/b/test-bucket/o/report";
const OVERRIDES: &str =
    "response-content-type=text%2Fcsv&response-content-disposition=inline%3B%20filename%3Dr.csv";

#[test]
fn json_object_reads_accept_and_ignore_response_content_overrides() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("report", b"report-bytes").json_ok();
    for path in [
        format!("{OBJECT}?alt=media"),
        "/download/storage/v1/b/test-bucket/o/report?alt=media".to_owned(),
    ] {
        let plain = server.request("GET", &path, "", b"");
        plain.expect(200);
        let overridden = server.request("GET", &format!("{path}&{OVERRIDES}"), "", b"");
        overridden.expect(200);
        assert_eq!(overridden.body, plain.body, "{path}");
        for header in ["content-type", "content-disposition"] {
            assert_eq!(
                overridden.headers[header], plain.headers[header],
                "{path} {header}"
            );
        }
        assert_eq!(overridden.headers["content-disposition"], "attachment");
    }
    let metadata = server.request("GET", OBJECT, "", b"").json_ok();
    let overridden = server
        .request("GET", &format!("{OBJECT}?{OVERRIDES}"), "", b"")
        .json_ok();
    assert_eq!(overridden, metadata);
    server
        .request("HEAD", &format!("{OBJECT}?alt=media&{OVERRIDES}"), "", b"")
        .expect(200);
}

#[test]
fn other_overrides_and_other_methods_are_ignored_like_any_undeclared_name() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    server.bucket();
    server.put("report", b"report-bytes").json_ok();
    // Object methods ignore every name they do not declare, these included (cases
    // obj-undeclared-query-writes, obj-list-undeclared-query and obj-delete-undeclared-query).
    for (method, path, body) in [
        ("GET", format!("{OBJECT}?response-content-language=en"), ""),
        (
            "PATCH",
            format!("{OBJECT}?response-content-type=text%2Fcsv"),
            "{}",
        ),
    ] {
        let reply = server.request(method, &path, "", body.as_bytes()).json_ok();
        assert_eq!(reply["name"], "report", "{method} {path}");
    }
    server
        .request(
            "GET",
            "/storage/v1/b/test-bucket/o?response-content-type=text%2Fcsv",
            "",
            b"",
        )
        .json_ok();
    server
        .request(
            "DELETE",
            &format!("{OBJECT}?response-content-disposition=inline"),
            "",
            b"",
        )
        .expect(204);
}
