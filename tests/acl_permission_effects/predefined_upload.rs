use super::*;
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

const UPLOAD: &str = "/upload/storage/v1/b/test-bucket/o";
const ADMIN_OWNER: (&str, &str) = ("user-admin@example.test", "OWNER");

fn acl(value: &Value) -> Vec<(String, String)> {
    let mut entries: Vec<_> = value["acl"]
        .as_array()
        .unwrap_or_else(|| panic!("no acl in {value}"))
        .iter()
        .map(|entry| {
            (
                entry["entity"].as_str().unwrap().to_owned(),
                entry["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    entries.sort_unstable();
    entries
}

fn pairs(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut entries: Vec<_> = entries
        .iter()
        .map(|(entity, role)| ((*entity).to_owned(), (*role).to_owned()))
        .collect();
    entries.sort_unstable();
    entries
}

/// The default object ACL after one insert: projectPrivate plus `entity`, then the owner.
fn inherited(entity: &str) -> Vec<(String, String)> {
    pairs(&[
        ADMIN_OWNER,
        ("project-owners-0", "OWNER"),
        ("project-editors-0", "OWNER"),
        ("project-viewers-0", "READER"),
        (entity, "READER"),
    ])
}

fn stored(server: &Server, name: &str) -> Vec<(String, String)> {
    acl(&server
        .request(
            "GET",
            &format!("{BUCKET}/o/{name}?projection=full"),
            ADMIN,
            b"",
        )
        .json())
}

fn absent(server: &Server, name: &str) {
    server
        .request("GET", &format!("{BUCKET}/o/{name}"), ADMIN, b"")
        .error(404, "notFound");
}

/// Sends a multipart upload, whose outer content type the shared JSON helper cannot set.
fn multipart(server: &Server, query: &str, name: &str) -> u16 {
    let body = format!(
        "--b\r\nContent-Type: application/json\r\n\r\n{{\"name\":\"{name}\"}}\r\n--b\r\nContent-Type: text/plain\r\n\r\nmultipart bytes\r\n--b--\r\n"
    );
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(
        stream,
        "POST {UPLOAD}?uploadType=multipart&{query} HTTP/1.1\r\nHost: localhost\r\n{ADMIN}Content-Type: multipart/related; boundary=b\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn initiate(server: &Server, query: &str) -> String {
    let initiated = server.request(
        "POST",
        &format!("{UPLOAD}?uploadType=resumable&{query}"),
        ADMIN,
        b"",
    );
    assert_eq!(
        initiated.status,
        200,
        "{}",
        String::from_utf8_lossy(&initiated.body)
    );
    let uri: hyper::Uri = initiated.headers["location"].parse().unwrap();
    uri.path_and_query().unwrap().as_str().to_owned()
}

fn finish(server: &Server, session: &str) -> Reply {
    server.request(
        "PUT",
        session,
        &format!("{ADMIN}Content-Range: bytes 0-4/5\r\n"),
        b"bytes",
    )
}

#[test]
fn media_upload_predefined_acls_replace_the_default_object_acl() {
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let inputs = TestDir::new();
        let server = Server::start(&data, &inputs, backend);
        server.bucket();
        // A stored default object ACL shows that predefinedAcl replaces it instead of merging.
        server
            .admin(
                "POST",
                &format!("{BUCKET}/defaultObjectAcl"),
                json!({"entity":"user-reader@example.test","role":"READER"}),
            )
            .json();
        let cases = [
            ("private", vec![ADMIN_OWNER]),
            (
                "projectPrivate",
                vec![
                    ADMIN_OWNER,
                    ("project-owners-0", "OWNER"),
                    ("project-editors-0", "OWNER"),
                    ("project-viewers-0", "READER"),
                ],
            ),
            (
                "bucketOwnerRead",
                vec![ADMIN_OWNER, ("project-owners-0", "READER")],
            ),
            (
                "bucketOwnerFullControl",
                vec![ADMIN_OWNER, ("project-owners-0", "OWNER")],
            ),
            (
                "authenticatedRead",
                vec![ADMIN_OWNER, ("allAuthenticatedUsers", "READER")],
            ),
            ("publicRead", vec![ADMIN_OWNER, ("allUsers", "READER")]),
        ];
        for (value, expected) in cases {
            let name = format!("media-{value}");
            let created = server
                .request(
                    "POST",
                    &format!(
                        "{UPLOAD}?uploadType=media&name={name}&predefinedAcl={value}&projection=full"
                    ),
                    ADMIN,
                    b"bytes",
                )
                .json();
            assert_eq!(acl(&created), pairs(&expected), "{backend} {value}");
            assert_eq!(
                stored(&server, &name),
                pairs(&expected),
                "{backend} {value}"
            );
        }
        server
            .request(
                "POST",
                &format!("{UPLOAD}?uploadType=media&name=inherited"),
                ADMIN,
                b"x",
            )
            .json();
        assert_eq!(
            stored(&server, "inherited"),
            inherited("user-reader@example.test")
        );
        // The canned grants take effect: public reads need no credentials, authenticated
        // reads need any identity, and private objects stay closed to both.
        let media = |name: &str, headers: &str| {
            server.request("GET", &format!("{BUCKET}/o/{name}?alt=media"), headers, b"")
        };
        assert_eq!(media("media-publicRead", "").body, b"bytes");
        assert_eq!(media("media-authenticatedRead", OTHER).body, b"bytes");
        assert_ne!(media("media-authenticatedRead", "").status, 200);
        assert_ne!(media("media-private", OTHER).status, 200);
        for value in ["publicReadWrite", "bogus", ""] {
            server
                .request(
                    "POST",
                    &format!("{UPLOAD}?uploadType=media&name=invalid&predefinedAcl={value}"),
                    ADMIN,
                    b"bytes",
                )
                .error(400, "invalidParameter");
        }
        absent(&server, "invalid");
    }
}

#[test]
fn multipart_and_resumable_uploads_apply_predefined_acl_at_publication() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let mut server = Server::start(&data, &inputs, "disk");
    server.bucket();
    assert_eq!(
        multipart(&server, "predefinedAcl=publicRead", "multipart"),
        200
    );
    assert_eq!(
        stored(&server, "multipart"),
        pairs(&[ADMIN_OWNER, ("allUsers", "READER")])
    );
    assert_eq!(multipart(&server, "predefinedAcl=bogus", "rejected"), 400);
    absent(&server, "rejected");

    let session = initiate(&server, "name=resumable&predefinedAcl=bucketOwnerRead");
    // The session persists the requested ACL: a later default change and a restart between
    // initiation and completion do not alter what completion applies.
    server
        .admin(
            "POST",
            &format!("{BUCKET}/defaultObjectAcl"),
            json!({"entity":"user-other@example.test","role":"READER"}),
        )
        .json();
    server.stop();
    let server = Server::start(&data, &inputs, "disk");
    finish(&server, &session).json();
    assert_eq!(
        stored(&server, "resumable"),
        pairs(&[ADMIN_OWNER, ("project-owners-0", "READER")])
    );
    let session = initiate(&server, "name=resumable-default");
    finish(&server, &session).json();
    assert_eq!(
        stored(&server, "resumable-default"),
        inherited("user-other@example.test")
    );

    // Uniform access enabled after initiation is rechecked when the session completes.
    let pending = initiate(&server, "name=late-uniform&predefinedAcl=private");
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}),
        )
        .json();
    for (reply, reason) in [
        (finish(&server, &pending), "invalid"),
        (
            server.request(
                "POST",
                &format!("{UPLOAD}?uploadType=media&name=late-uniform&predefinedAcl=private"),
                ADMIN,
                b"bytes",
            ),
            "invalidArgument",
        ),
    ] {
        reply.error(400, reason);
        assert!(
            serde_json::from_slice::<Value>(&reply.body).unwrap()["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("Cannot insert legacy ACL for an object when uniform bucket-level access is enabled")
        );
    }
    absent(&server, "late-uniform");
}

#[test]
fn predefined_acl_uploads_need_only_create_and_respect_access_settings() {
    let data = TestDir::new();
    let inputs = TestDir::new();
    let server = Server::start(&data, &inputs, "disk");
    server.bucket();
    // WRITER grants storage.objects.create but no object setIamPolicy, which the documented
    // insert permissions do not require for predefinedAcl.
    super::bucket::grant(&server, "user-reader@example.test", "WRITER");
    server
        .request(
            "POST",
            &format!("{UPLOAD}?uploadType=media&name=writer&predefinedAcl=publicRead"),
            READER,
            b"bytes",
        )
        .json();
    assert_eq!(
        stored(&server, "writer"),
        pairs(&[
            ("user-reader@example.test", "OWNER"),
            ("allUsers", "READER")
        ])
    );
    super::bucket::grant(&server, "allUsers", "WRITER");
    let anonymous = |query: &str| {
        server.request(
            "POST",
            &format!("{UPLOAD}?uploadType=media&name=anonymous{query}"),
            "",
            b"bytes",
        )
    };
    anonymous("&predefinedAcl=private").error(400, "invalidArgument");
    absent(&server, "anonymous");
    anonymous("").json();

    // A session initiated before public access prevention is enforced is rechecked at
    // completion, inside the publishing transaction.
    let pending = initiate(&server, "name=late-public&predefinedAcl=publicRead");
    server
        .admin(
            "PATCH",
            BUCKET,
            json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
        )
        .json();
    finish(&server, &pending).error(412, "conditionNotMet");
    absent(&server, "late-public");
    for value in ["publicRead", "authenticatedRead"] {
        server
            .request(
                "POST",
                &format!("{UPLOAD}?uploadType=media&name=public&predefinedAcl={value}"),
                ADMIN,
                b"bytes",
            )
            .error(412, "conditionNotMet");
        absent(&server, "public");
    }
    server
        .request(
            "POST",
            &format!("{UPLOAD}?uploadType=media&name=private&predefinedAcl=private"),
            ADMIN,
            b"bytes",
        )
        .json();
    assert_eq!(stored(&server, "private"), pairs(&[ADMIN_OWNER]));

    // The ACL row commits with the generation: a failed ACL write publishes nothing.
    let db = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_acl BEFORE INSERT ON acl_object_entries WHEN NEW.name='atomic' BEGIN SELECT RAISE(ABORT,'injected ACL failure'); END;").unwrap();
    let failed = server.request(
        "POST",
        &format!("{UPLOAD}?uploadType=media&name=atomic&predefinedAcl=projectPrivate"),
        ADMIN,
        b"bytes",
    );
    assert_eq!(
        failed.status,
        503,
        "{}",
        String::from_utf8_lossy(&failed.body)
    );
    absent(&server, "atomic");
}
