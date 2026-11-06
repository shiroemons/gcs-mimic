use super::*;

const IDENTITIES: usize = 257;
const TOKENS: usize = 17;
/// Their upload grants alone would overflow the 64 KiB that stored upload options may hold.
const PROJECTS: usize = 2000;
const LAST: &str = "Authorization: Bearer token-256-16\r\n";

fn large_registry() -> Value {
    let identities: Vec<_> = (0..IDENTITIES)
        .map(|index| {
            let tokens: Vec<_> = (0..TOKENS)
                .map(|token| format!("token-{index}-{token}"))
                .collect();
            if index + 1 < IDENTITIES {
                return json!({"id": format!("user-{index}@example.invalid"), "tokens": tokens});
            }
            let projects: serde_json::Map<_, _> = (0..PROJECTS)
                .map(|project| {
                    (
                        format!("project-{project:04}"),
                        json!(["roles/storage.admin"]),
                    )
                })
                .collect();
            json!({
                "id": format!("user-{index}@example.invalid"),
                "tokens": tokens,
                "project_roles": projects,
            })
        })
        .collect();
    json!({
        "identities": identities,
        "public_access_prevention_projects": (0..65)
            .map(|project| format!("parent-{project}"))
            .collect::<Vec<_>>(),
    })
}

fn strict_large(data: &TestDir, identities: &Path) -> Server {
    let (server, ready) = Server::start(
        data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    assert_eq!(ready["identities"], IDENTITIES);
    server
}

fn initiate(server: &Server, name: &str) -> String {
    let initiated = server.request(
        "POST",
        &format!("/upload/storage/v1/b/large-registry/o?uploadType=resumable&name={name}"),
        LAST,
        b"",
    );
    initiated.expect(200);
    let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
    uri.path_and_query().unwrap().as_str().to_owned()
}

/// A registry past the former limits of 256 identities, 16 tokens, 64 projects and 64 parent
/// policy projects has to start, authorize a principal's last token and carry a resumable upload
/// across a restart, which stores that principal's grants within a 64 KiB column.
#[test]
fn a_registry_past_the_former_limits_authorizes_uploads_across_a_restart() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let identities = common::process::write_identities(&inputs.0, &large_registry());
    let mut server = strict_large(&data, &identities);
    server
        .create_bucket(LAST, "project-1999", "large-registry")
        .expect(200);
    server
        .upload(LAST, "large-registry", "media", b"media")
        .expect(200);
    let session = initiate(&server, "resumed");
    server.stop();

    let server = strict_large(&data, &identities);
    server
        .request(
            "PUT",
            &session,
            &format!("{LAST}Content-Range: bytes 0-4/5\r\n"),
            b"bytes",
        )
        .expect(200);
    let read = server.get(LAST, "/storage/v1/b/large-registry/o/resumed?alt=media");
    read.expect(200);
    assert_eq!(read.body, b"bytes");
    let other = "Authorization: Bearer token-0-16\r\n";
    assert_eq!(
        server
            .get(other, "/storage/v1/b/large-registry/o/media")
            .status,
        403
    );
}
