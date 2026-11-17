//! A request names its project by ID or by the number its resources render (recorded
//! 2026-11-17). Project `local`'s number is derived from its ID.

use crate::common;
use common::TestDir;
use common::effects::Server;
use serde_json::Value;

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const LOCAL: &str = "902648055030";

fn create(server: &Server, project: &str, name: &str) -> Value {
    server
        .request(
            "POST",
            &format!("/storage/v1/b?project={project}"),
            ADMIN,
            format!(r#"{{"name":"{name}"}}"#).as_bytes(),
        )
        .json_ok()
}

fn list(server: &Server, query: &str) -> Value {
    server
        .request("GET", &format!("/storage/v1/b?{query}"), ADMIN, b"")
        .json_ok()
}

fn names(listing: &Value) -> Vec<&str> {
    listing["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| item["name"].as_str().unwrap())
        .collect()
}

#[test]
fn a_project_number_names_the_project_its_resources_render() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    let by_id = create(&server, "local", "by-id");
    assert_eq!(by_id["projectNumber"], LOCAL);
    assert_eq!(create(&server, LOCAL, "by-number")["projectNumber"], LOCAL);
    for project in ["local", LOCAL] {
        let listing = list(&server, &format!("project={project}"));
        assert_eq!(names(&listing), ["by-id", "by-number"], "{project}");
    }
    // A page token continues whichever spelling the next page uses.
    let first = list(&server, "project=local&maxResults=1");
    assert_eq!(names(&first), ["by-id"]);
    let token = first["nextPageToken"].as_str().unwrap();
    let second = list(&server, &format!("project={LOCAL}&pageToken={token}"));
    assert_eq!(names(&second), ["by-number"]);
    // The service agent and HMAC keys are the same project's too.
    let agent = server
        .request(
            "GET",
            &format!("/storage/v1/projects/{LOCAL}/serviceAccount"),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(
        agent["email_address"],
        format!("service-{LOCAL}@gs-project-accounts.iam.gserviceaccount.com")
    );
    let key = server
        .request(
            "POST",
            &format!(
                "/storage/v1/projects/{LOCAL}/hmacKeys?serviceAccountEmail=sa%40example.invalid"
            ),
            ADMIN,
            b"",
        )
        .json_ok();
    assert_eq!(key["metadata"]["projectId"], "local");
    let keys = server
        .request("GET", "/storage/v1/projects/local/hmacKeys", ADMIN, b"")
        .json_ok();
    assert_eq!(keys["items"][0]["accessId"], key["metadata"]["accessId"]);
    // The XML API takes the number in `x-goog-project-id` as well.
    let xml = server.request("GET", "/", &format!("x-goog-project-id: {LOCAL}\r\n"), b"");
    let text = String::from_utf8_lossy(&xml.body);
    assert_eq!(xml.status, 200, "{text}");
    assert!(text.contains("<Name>by-number</Name>"), "{text}");
}

#[test]
fn a_number_no_known_project_has_names_a_project_of_its_own() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    create(&server, "local", "by-id");
    let other = create(&server, "123456789012", "elsewhere");
    assert_eq!(other["projectNumber"], "123456789012");
    assert_eq!(names(&list(&server, "project=local")), ["by-id"]);
    assert_eq!(names(&list(&server, "project=123456789012")), ["elsewhere"]);
}

#[test]
fn strict_mode_reaches_a_granted_project_by_number_before_it_owns_anything() {
    let dir = TestDir::new();
    let server = Server::start(&dir, true);
    // The identity file grants roles in `local`, so its number is known with an empty catalog
    // and the grant authorizes the request.
    create(&server, LOCAL, "first");
    assert_eq!(names(&list(&server, "project=local")), ["first"]);
    let viewer = server.request(
        "POST",
        &format!("/storage/v1/b?project={LOCAL}"),
        "Authorization: Bearer viewer\r\n",
        br#"{"name":"refused"}"#,
    );
    viewer.expect(403);
}
