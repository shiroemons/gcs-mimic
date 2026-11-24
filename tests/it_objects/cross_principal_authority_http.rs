//! Capabilities that outlive one request, resumable session URIs and rewrite tokens, do not
//! carry authority across buckets or principals. Two principals own one bucket each through
//! bucket IAM and hold no project role, so every grant comes from the bucket policy.

use crate::common;
use common::TestDir;
use common::effects::{Reply, Server};
use serde_json::json;

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const ALICE: &str = "Authorization: Bearer alice\r\n";
const BOB: &str = "Authorization: Bearer bob\r\n";
const MIB: usize = 1024 * 1024;

fn start(dir: &TestDir) -> Server {
    let identities = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"admin@example.test","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"alice@example.test","tokens":["alice"]},
            {"id":"bob@example.test","tokens":["bob"]}
        ]}),
    );
    let server = Server::start_with_args(
        dir,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
    );
    for (bucket, owner) in [("alice-bucket", "alice"), ("bob-bucket", "bob")] {
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                ADMIN,
                json!({"name": bucket}).to_string().as_bytes(),
            )
            .json_ok();
        server
            .request(
                "PUT",
                &format!("/storage/v1/b/{bucket}/iam"),
                ADMIN,
                json!({"bindings":[{
                    "role":"roles/storage.objectAdmin",
                    "members":[format!("user:{owner}@example.test")]
                }]})
                .to_string()
                .as_bytes(),
            )
            .json_ok();
    }
    server
}

fn upload(server: &Server, auth: &str, bucket: &str, name: &str, body: &[u8]) -> Reply {
    server.request(
        "POST",
        &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}"),
        auth,
        body,
    )
}

fn object_status(server: &Server, auth: &str, bucket: &str, name: &str) -> u16 {
    server
        .request(
            "GET",
            &format!("/storage/v1/b/{bucket}/o/{name}"),
            auth,
            b"",
        )
        .status
}

#[test]
fn resumable_session_uri_is_bound_to_its_bucket_and_name() {
    let dir = TestDir::new();
    let server = start(&dir);
    let initiated = server.request(
        "POST",
        "/upload/storage/v1/b/alice-bucket/o?uploadType=resumable",
        ALICE,
        br#"{"name":"original"}"#,
    );
    initiated.expect(200);
    let location = &initiated.headers["location"];
    let path = location.strip_prefix("http://127.0.0.1:4443").unwrap();
    let id = path.split("upload_id=").nth(1).unwrap().to_owned();
    let chunk = "Content-Range: bytes 0-4/5\r\n";

    // Sessions are bearer capabilities, so the replay carries no Authorization header and
    // still must not reach another bucket.
    let json = server.request(
        "PUT",
        &format!("/upload/storage/v1/b/bob-bucket/o?uploadType=resumable&upload_id={id}"),
        chunk,
        b"hello",
    );
    assert_eq!(json.status, 404, "{}", String::from_utf8_lossy(&json.body));
    let xml = server.request(
        "PUT",
        &format!("/bob-bucket/original?upload_id={id}"),
        chunk,
        b"hello",
    );
    xml.expect(404);
    assert!(
        String::from_utf8_lossy(&xml.body).contains("<Code>NoSuchUpload</Code>"),
        "{}",
        String::from_utf8_lossy(&xml.body)
    );
    assert_eq!(object_status(&server, BOB, "bob-bucket", "original"), 404);
    let listing = server
        .request("GET", "/storage/v1/b/bob-bucket/o", BOB, b"")
        .json_ok();
    assert!(listing.get("items").is_none(), "{listing}");

    // The failed replays leave the session usable, and a name in the query does not rename it.
    let stored = server
        .request("PUT", &format!("{path}&name=other"), chunk, b"hello")
        .json_ok();
    assert_eq!(stored["name"], "original");
    assert_eq!(stored["bucket"], "alice-bucket");
    assert_eq!(
        object_status(&server, ALICE, "alice-bucket", "original"),
        200
    );
    assert_eq!(object_status(&server, ALICE, "alice-bucket", "other"), 404);
}

#[test]
fn rewrite_tokens_do_not_carry_authority_across_principals() {
    let dir = TestDir::new();
    let server = start(&dir);
    let big = vec![b'x'; MIB + 5];
    upload(&server, BOB, "bob-bucket", "source", &big).json_ok();
    upload(&server, ALICE, "alice-bucket", "source", &big).json_ok();
    // A storage class change keeps the rewrite from finishing within one call.
    let begin = |auth: &str, bucket: &str| {
        let first = server
            .request(
                "POST",
                &format!(
                    "/storage/v1/b/{bucket}/o/source/rewriteTo/b/{bucket}/o/copy?maxBytesRewrittenPerCall=1048576"
                ),
                auth,
                br#"{"storageClass":"NEARLINE"}"#,
            )
            .json_ok();
        assert_eq!(first["done"], false, "{first}");
        first["rewriteToken"].as_str().unwrap().to_owned()
    };
    let bob_token = begin(BOB, "bob-bucket");
    let alice_token = begin(ALICE, "alice-bucket");
    let rewrite = |source: &str, destination: &str, token: &str| {
        server.request(
            "POST",
            &format!(
                "/storage/v1/b/{source}/o/source/rewriteTo/b/{destination}/o/copy?maxBytesRewrittenPerCall=1048576&rewriteToken={token}"
            ),
            ALICE,
            br#"{"storageClass":"NEARLINE"}"#,
        )
    };
    let denied = |reply: Reply| {
        let error = reply.json_reason_value(403, "forbidden");
        let message = error["error"]["message"].as_str().unwrap().to_owned();
        assert!(message.contains("storage.objects.get"), "{message}");
    };
    // (a) Bob's token on bob's own source and destination.
    denied(rewrite("bob-bucket", "bob-bucket", &bob_token));
    // (b) Bob's token on bob's source with alice's destination.
    denied(rewrite("bob-bucket", "alice-bucket", &bob_token));
    // (c) Alice's own token on a URL whose source is bob's object.
    denied(rewrite("bob-bucket", "alice-bucket", &alice_token));
    assert_eq!(object_status(&server, ALICE, "alice-bucket", "copy"), 404);
    assert_eq!(object_status(&server, BOB, "bob-bucket", "copy"), 404);

    // The owners still finish their own rewrites with their own tokens.
    let finished = server
        .request(
            "POST",
            &format!(
                "/storage/v1/b/bob-bucket/o/source/rewriteTo/b/bob-bucket/o/copy?maxBytesRewrittenPerCall=1048576&rewriteToken={bob_token}"
            ),
            BOB,
            br#"{"storageClass":"NEARLINE"}"#,
        )
        .json_ok();
    assert_eq!(finished["done"], true, "{finished}");
}
