mod object_effects_support;

use object_effects_support::{Server, common::TestDir};
use serde_json::{Value, json};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const KEYS: &str = "/storage/v1/projects/local/hmacKeys";

fn create(server: &Server, email: &str) -> Value {
    server
        .request(
            "POST",
            &format!("{KEYS}?serviceAccountEmail={email}"),
            ADMIN,
            b"",
        )
        .json()
}

fn key_path(access_id: &str) -> String {
    format!("{KEYS}/{access_id}")
}

fn set_state(server: &Server, access_id: &str, body: Value) -> object_effects_support::Reply {
    server.request(
        "PUT",
        &key_path(access_id),
        ADMIN,
        body.to_string().as_bytes(),
    )
}

fn listed(server: &Server, query: &str) -> Vec<String> {
    let value = server
        .request("GET", &format!("{KEYS}?{query}"), ADMIN, b"")
        .json();
    assert_eq!(value["kind"], "storage#hmacKeysMetadata");
    value["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["accessId"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn hmac_key_lifecycle_state_rules_etags_and_restart() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, false);
    let created = create(&server, "sa%40example.invalid");
    assert_eq!(created["kind"], "storage#hmacKey");
    let secret = created["secret"].as_str().unwrap();
    assert_eq!(secret.len(), 40);
    let metadata = &created["metadata"];
    let access_id = metadata["accessId"].as_str().unwrap().to_owned();
    assert_eq!(access_id.len(), 61);
    assert!(access_id.starts_with("GOOG1E"));
    assert_eq!(metadata["kind"], "storage#hmacKeyMetadata");
    assert_eq!(metadata["id"], format!("local/{access_id}"));
    assert_eq!(
        metadata["selfLink"],
        format!("http://127.0.0.1:4443/storage/v1/projects/local/hmacKeys/{access_id}")
    );
    assert_eq!(metadata["projectId"], "local");
    assert_eq!(metadata["serviceAccountEmail"], "sa@example.invalid");
    assert_eq!(metadata["state"], "ACTIVE");
    assert_eq!(metadata["timeCreated"], metadata["updated"]);
    let first_etag = metadata["etag"].as_str().unwrap().to_owned();

    // The secret is returned only by create.
    let fetched = server
        .request("GET", &key_path(&access_id), ADMIN, b"")
        .json();
    assert_eq!(&fetched, metadata);
    assert!(fetched.get("secret").is_none());
    let partial = server
        .request(
            "GET",
            &format!("{}?fields=accessId,state", key_path(&access_id)),
            ADMIN,
            b"",
        )
        .json();
    assert_eq!(partial, json!({"accessId": access_id, "state": "ACTIVE"}));

    server
        .request("DELETE", &key_path(&access_id), ADMIN, b"")
        .error(400, "badRequest");
    set_state(
        &server,
        &access_id,
        json!({"state": "INACTIVE", "etag": "stale"}),
    )
    .error(412, "conditionNotMet");
    set_state(&server, &access_id, json!({"state": "DELETED"})).error(400, "invalidArgument");
    set_state(&server, &access_id, json!({"etag": first_etag})).error(400, "required");
    assert_eq!(
        server
            .request("GET", &key_path(&access_id), ADMIN, b"")
            .json()["state"],
        "ACTIVE"
    );

    // Read-modify-write clients send back the whole metadata resource.
    let mut round_trip = fetched;
    round_trip["state"] = "INACTIVE".into();
    let inactive = set_state(&server, &access_id, round_trip).json();
    assert_eq!(inactive["state"], "INACTIVE");
    assert_ne!(inactive["etag"], first_etag);
    assert_eq!(inactive["timeCreated"], metadata["timeCreated"]);
    set_state(
        &server,
        &access_id,
        json!({"state": "ACTIVE", "etag": first_etag}),
    )
    .error(412, "conditionNotMet");

    let reply = server.request("DELETE", &key_path(&access_id), ADMIN, b"");
    assert_eq!(
        reply.status,
        204,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let deleted = server
        .request("GET", &key_path(&access_id), ADMIN, b"")
        .json();
    assert_eq!(deleted["state"], "DELETED");
    assert_ne!(deleted["etag"], inactive["etag"]);
    set_state(&server, &access_id, json!({"state": "ACTIVE"})).error(400, "badRequest");
    server
        .request("DELETE", &key_path(&access_id), ADMIN, b"")
        .error(400, "badRequest");
    assert!(listed(&server, "").is_empty());
    assert_eq!(
        listed(&server, "showDeletedKeys=true"),
        std::slice::from_ref(&access_id)
    );

    let other = create(&server, "other%40example.invalid");
    let other_id = other["metadata"]["accessId"].as_str().unwrap().to_owned();
    server
        .request(
            "GET",
            &format!("/storage/v1/projects/elsewhere/hmacKeys/{other_id}"),
            ADMIN,
            b"",
        )
        .error(404, "notFound");
    server
        .request("GET", &key_path("GOOG1EMISSING"), ADMIN, b"")
        .error(404, "notFound");
    set_state(&server, "GOOG1EMISSING", json!({"state": "ACTIVE"})).error(404, "notFound");

    server.stop();
    let server = Server::start(&dir, false);
    let restored = server
        .request("GET", &key_path(&access_id), ADMIN, b"")
        .json();
    assert_eq!(restored, deleted);
    assert_eq!(listed(&server, ""), std::slice::from_ref(&other_id));
    let catalog = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let secrets: Vec<(String, Option<String>)> = catalog
        .prepare("SELECT access_id, secret FROM hmac_keys ORDER BY access_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for (id, stored) in secrets {
        // Deletion erases secret material; live keys keep it for later verification.
        assert_eq!(stored.is_none(), id == access_id, "{id}");
    }
}

#[test]
fn hmac_key_listing_filters_pages_and_limits() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    let mut first: Vec<String> = (0..3)
        .map(|_| {
            create(&server, "a%40example.invalid")["metadata"]["accessId"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    first.sort();
    let second = create(&server, "b%40example.invalid")["metadata"]["accessId"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        listed(&server, "serviceAccountEmail=a%40example.invalid"),
        first
    );
    assert_eq!(
        listed(&server, "serviceAccountEmail=b%40example.invalid"),
        std::slice::from_ref(&second)
    );
    assert_eq!(listed(&server, "").len(), 4);

    let mut paged = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let query = match &token {
            Some(token) => {
                format!("serviceAccountEmail=a%40example.invalid&maxResults=2&pageToken={token}")
            }
            None => "serviceAccountEmail=a%40example.invalid&maxResults=2".into(),
        };
        let page = server
            .request("GET", &format!("{KEYS}?{query}"), ADMIN, b"")
            .json();
        let items = page["items"].as_array().unwrap();
        assert!(items.len() <= 2);
        paged.extend(
            items
                .iter()
                .map(|item| item["accessId"].as_str().unwrap().to_owned()),
        );
        match page["nextPageToken"].as_str() {
            Some(next) => token = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(paged, first);
    let page = server
        .request(
            "GET",
            &format!("{KEYS}?serviceAccountEmail=a%40example.invalid&maxResults=1"),
            ADMIN,
            b"",
        )
        .json();
    let token = page["nextPageToken"].as_str().unwrap();
    // A token is bound to the filters it was issued for.
    server
        .request(
            "GET",
            &format!("{KEYS}?maxResults=1&pageToken={token}"),
            ADMIN,
            b"",
        )
        .error(400, "invalidParameter");
    assert!(
        server
            .request("GET", "/storage/v1/projects/elsewhere/hmacKeys", ADMIN, b"")
            .json()["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // At most ten keys that are not DELETED exist per service account.
    for _ in 0..7 {
        create(&server, "a%40example.invalid");
    }
    server
        .request(
            "POST",
            &format!("{KEYS}?serviceAccountEmail=a%40example.invalid"),
            ADMIN,
            b"",
        )
        .error(400, "badRequest");
    assert_eq!(
        listed(&server, "serviceAccountEmail=a%40example.invalid").len(),
        10
    );
    set_state(&server, &first[0], json!({"state": "INACTIVE"})).json();
    assert_eq!(
        server
            .request("DELETE", &key_path(&first[0]), ADMIN, b"")
            .status,
        204
    );
    create(&server, "a%40example.invalid");

    server
        .request(
            "POST",
            &format!("{KEYS}?serviceAccountEmail=c%40example.invalid"),
            ADMIN,
            b"{}",
        )
        .error(400, "invalidArgument");
    server
        .request("POST", KEYS, ADMIN, b"")
        .error(400, "required");
    server
        .request("PATCH", &key_path(&second), ADMIN, b"{}")
        .error(405, "methodNotAllowed");
}

/// POST with an explicit content type, as the pinned Ruby client sends create.
fn form_create(server: &Server, query: &str, body: &str) -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let mut stream = std::net::TcpStream::connect(&server.address).unwrap();
    write!(
        stream,
        "POST {KEYS}?{query} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[test]
fn create_accepts_only_form_parameters_identical_to_the_query() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    let query = "serviceAccountEmail=form%40example.invalid";
    assert_eq!(form_create(&server, query, query), 200);
    for body in [
        "serviceAccountEmail=other%40example.invalid",
        "serviceAccountEmail=form%40example.invalid&extra=1",
        "not a form",
    ] {
        assert_eq!(form_create(&server, query, body), 400, "{body}");
    }
    assert_eq!(
        listed(&server, "serviceAccountEmail=form%40example.invalid").len(),
        1
    );
}

#[test]
fn service_account_is_deterministic_per_project() {
    let dir = TestDir::new();
    let server = Server::start(&dir, false);
    let value = server
        .request("GET", "/storage/v1/projects/123456/serviceAccount", "", b"")
        .json();
    assert_eq!(
        value,
        json!({"kind": "storage#serviceAccount", "email_address": "service-123456@gs-project-accounts.iam.gserviceaccount.com"})
    );
    let named = server
        .request(
            "GET",
            "/storage/v1/projects/local-project/serviceAccount",
            "",
            b"",
        )
        .json();
    assert_eq!(
        named["email_address"],
        "service-0@gs-project-accounts.iam.gserviceaccount.com"
    );
    server
        .request(
            "GET",
            "/storage/v1/projects/p/serviceAccount?userProject=p",
            "",
            b"",
        )
        .error(400, "badRequest");
}

/// A strict server whose principals separate HMAC key administration from storage
/// administration, as the predefined roles do.
struct StrictServer {
    child: std::process::Child,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    address: String,
}

impl StrictServer {
    fn start(dir: &TestDir) -> Self {
        use std::io::BufRead;
        let identities = dir.0.join("identities.json");
        std::fs::write(
            &identities,
            json!({"identities":[
                {"id":"keys@example.test","tokens":["keys"],"project_roles":{"local":["roles/storage.hmacKeyAdmin"]}},
                {"id":"reader@example.test","tokens":["reader"],"project_roles":{"local":["roles/storage.viewer"]}},
                {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}}
            ]})
            .to_string(),
        )
        .unwrap();
        let mut child = object_effects_support::common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args([
                "--api-base-url",
                "http://127.0.0.1:4443",
                "--auth-mode",
                "strict",
                "--identity-file",
            ])
            .arg(identities)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
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

    fn request(
        &self,
        method: &str,
        path: &str,
        token: &str,
        body: &[u8],
    ) -> object_effects_support::Reply {
        let headers = if token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {token}\r\n")
        };
        object_effects_support::request(&self.address, method, path, &headers, body)
    }
}

impl Drop for StrictServer {
    fn drop(&mut self) {
        object_effects_support::common::stop_server(&mut self.child, &mut self.stdout);
    }
}

#[test]
fn strict_profile_authorizes_project_resources() {
    let dir = TestDir::new();
    let server = StrictServer::start(&dir);
    let created = server
        .request(
            "POST",
            &format!("{KEYS}?serviceAccountEmail=sa%40example.invalid"),
            "keys",
            b"",
        )
        .json();
    let access_id = created["metadata"]["accessId"].as_str().unwrap().to_owned();
    let create_path = format!("{KEYS}?serviceAccountEmail=sa%40example.invalid");
    // roles/storage.viewer grants hmacKeys get/list only; roles/storage.admin grants none.
    assert_eq!(
        listed_as(&server, "reader"),
        std::slice::from_ref(&access_id)
    );
    server
        .request("GET", &key_path(&access_id), "reader", b"")
        .json();
    for token in ["reader", "admin"] {
        server
            .request("POST", &create_path, token, b"")
            .error(403, "forbidden");
        server
            .request(
                "PUT",
                &key_path(&access_id),
                token,
                br#"{"state":"INACTIVE"}"#,
            )
            .error(403, "forbidden");
        server
            .request("DELETE", &key_path(&access_id), token, b"")
            .error(403, "forbidden");
    }
    server
        .request("GET", KEYS, "admin", b"")
        .error(403, "forbidden");
    // Service agent lookup needs only project visibility, which any local grant provides.
    for token in ["keys", "reader", "admin"] {
        assert_eq!(
            server
                .request(
                    "GET",
                    "/storage/v1/projects/local/serviceAccount",
                    token,
                    b""
                )
                .json()["kind"],
            "storage#serviceAccount"
        );
    }
    for (token, status, reason) in [("", 401, "required"), ("unknown", 401, "authError")] {
        for (method, path, body) in [
            (
                "POST",
                format!("{KEYS}?serviceAccountEmail=sa%40example.invalid"),
                "",
            ),
            ("GET", KEYS.to_owned(), ""),
            ("GET", key_path(&access_id), ""),
            ("PUT", key_path(&access_id), r#"{"state":"INACTIVE"}"#),
            ("DELETE", key_path(&access_id), ""),
            (
                "GET",
                "/storage/v1/projects/local/serviceAccount".to_owned(),
                "",
            ),
        ] {
            server
                .request(method, &path, token, body.as_bytes())
                .error(status, reason);
        }
    }
    // Project grants are evaluated in the path project.
    for path in [
        "/storage/v1/projects/elsewhere/hmacKeys".to_owned(),
        format!("/storage/v1/projects/elsewhere/hmacKeys/{access_id}"),
        "/storage/v1/projects/elsewhere/serviceAccount".to_owned(),
    ] {
        server
            .request("GET", &path, "keys", b"")
            .error(403, "forbidden");
    }
    // Authorization does not bypass the state rules.
    server
        .request("DELETE", &key_path(&access_id), "keys", b"")
        .error(400, "badRequest");
    assert_eq!(
        server
            .request(
                "PUT",
                &key_path(&access_id),
                "keys",
                br#"{"state":"INACTIVE"}"#
            )
            .json()["state"],
        "INACTIVE"
    );
    assert_eq!(
        server
            .request("DELETE", &key_path(&access_id), "keys", b"")
            .status,
        204
    );
}

fn listed_as(server: &StrictServer, token: &str) -> Vec<String> {
    server.request("GET", KEYS, token, b"").json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["accessId"].as_str().unwrap().to_owned())
        .collect()
}
