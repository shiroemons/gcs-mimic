use super::*;
use rusqlite::{Connection, params};
use std::io::{Read, Write};

const RECOVERY_ID: &str = "aabbccddeeff00112233445566778899";
const RETAINED_SIZE: u64 = (1 << 40) + 1;
const RETAINED_PREFIX: &[u8] = b"retained\0\xff";

// This stopped-catalog fixture models the Armed survivor in the storage recovery tests.
// The real server must inspect/protect it; it is not evidence of the originating I/O fault.
fn interrupted_workspace(data: &TestDir, tree: &TestDir) {
    let path = tree.0.join(".gcs-mimic-sync-work").join(RECOVERY_ID);
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(RETAINED_PREFIX).unwrap();
    file.set_len(RETAINED_SIZE).unwrap();
    file.sync_all().unwrap();
    fs::File::open(path.parent().unwrap())
        .unwrap()
        .sync_all()
        .unwrap();
    let connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(connection.execute("INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) SELECT ?1,w.root_id,b.project,s.bucket,s.incarnation,s.name,s.path,s.generation,0,1,2,COALESCE(w.sequence,0),COALESCE(w.epoch,0),?2,s.size,s.md5,s.crc32c FROM sync_objects s JOIN buckets b ON b.name=s.bucket CROSS JOIN sync_writeback_roots w WHERE s.bucket='test-bucket' AND s.name='body'", params![RECOVERY_ID, 10_u64.to_be_bytes()]).unwrap(), 1);
    assert_eq!(
        connection
            .execute(
                "UPDATE sync_writeback_roots SET entries=1,reserved=?1,recovery=1",
                [10_u128.to_be_bytes()]
            )
            .unwrap(),
        1
    );
}

fn retained(tree: &TestDir) {
    let file = fs::File::open(tree.0.join(".gcs-mimic-sync-work").join(RECOVERY_ID)).unwrap();
    assert_eq!(file.metadata().unwrap().len(), RETAINED_SIZE);
    let mut prefix = Vec::new();
    file.take(RETAINED_PREFIX.len() as u64)
        .read_to_end(&mut prefix)
        .unwrap();
    assert_eq!(prefix, RETAINED_PREFIX);
}

fn inspect(server: &Server, token: &str) -> Value {
    let status = server.cli_with_token("status", &[], Some(token), true);
    let page = server.cli_with_token(
        "findings",
        &["--revision", status["findings_revision"].as_str().unwrap()],
        Some(token),
        true,
    );
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["recovery_id"] == RECOVERY_ID)
        .unwrap()
        .clone()
}

fn resolution<'a>(item: &'a Value, operation: &'a str) -> Vec<&'a str> {
    vec![
        "--recovery",
        RECOVERY_ID,
        "--revision",
        item["recovery_revision"].as_str().unwrap(),
        "--target-identity",
        item["target_identity"].as_str().unwrap(),
        "--action",
        "discard",
        "--operation-id",
        operation,
    ]
}

#[test]
fn live_cli_inspects_stopped_sparse_recovery_and_requires_exact_confirmed_discard() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let inputs = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/body"), b"API winner").unwrap();
    let mut server = Server::start_with(&data, Some(&tree), &["--seed-project", "original"]);
    settle(&server, None);
    let winner = server
        .request("GET", "/storage/v1/b/test-bucket/o/body", None, b"")
        .json();
    server.stop();
    interrupted_workspace(&data, &tree);
    let registry = inputs.0.join("identities.json");
    fs::write(&registry, br#"{"identities":[{"id":"all","tokens":["all"],"project_roles":{"local":["roles/storage.admin"],"original":["roles/storage.admin"]}},{"id":"foreign","tokens":["foreign"],"project_roles":{"local":["roles/storage.admin"]}}]}"#).unwrap();
    let options = [
        "--auth-mode",
        "strict",
        "--identity-file",
        registry.to_str().unwrap(),
    ];
    let mut server = Server::start_with(&data, Some(&tree), &options);
    assert_eq!(
        server.cli_with_token("wait", &["--timeout", "1s"], Some("all"), false)["reason"],
        "Paused"
    );
    let item = inspect(&server, "all");
    assert_eq!(item["retained_bytes"], RETAINED_SIZE);
    assert_eq!(item["over_limit"], true);
    assert!(item["hashes"].is_null());
    assert_eq!(
        server.cli_with_token("wait", &["--timeout", "1s"], Some("all"), false)["completed"],
        false
    );
    retained(&tree);
    server.stop();
    let mut server = Server::start_with(&data, Some(&tree), &options);
    assert_eq!(
        server.cli_with_token("wait", &["--timeout", "1s"], Some("all"), false)["reason"],
        "Paused"
    );
    retained(&tree);
    let selected = inspect(&server, "all");
    assert_ne!(selected["recovery_revision"], item["recovery_revision"]);
    let mut stale = resolution(&item, "stale-recovery");
    stale.extend(["--confirm-backup-verified", RECOVERY_ID]);
    assert_eq!(
        server.cli_with_token("resolve", &stale, Some("all"), false)["error"]["code"],
        409
    );
    let mut exact = resolution(&selected, "discard-workspace");
    exact.extend(["--confirm-discard-without-backup", RECOVERY_ID]);
    assert_eq!(
        server.cli_with_token("resolve", &exact, Some("foreign"), false)["error"]["code"],
        403
    );
    let mut wrong_target = exact.clone();
    wrong_target[1] = "00000000000000000000000000000000";
    wrong_target[11] = wrong_target[1];
    assert_eq!(
        server.cli_with_token("resolve", &wrong_target, Some("all"), false)["error"]["code"],
        409
    );
    let mut wrong = exact.clone();
    wrong[5] = "00000000000000000000000000000000";
    assert_eq!(
        server.cli_with_token("resolve", &wrong, Some("all"), false)["error"]["code"],
        409
    );
    for confirmation in [
        vec![],
        vec![
            "--confirm-backup-verified",
            "00000000000000000000000000000000",
        ],
        vec![
            "--confirm-backup-verified",
            RECOVERY_ID,
            "--confirm-discard-without-backup",
            RECOVERY_ID,
        ],
    ] {
        let mut invalid = resolution(&selected, "unconfirmed");
        invalid.extend(confirmation);
        let output = common::server_command()
            .env("GCS_MIMIC_SYNC_TOKEN", "all")
            .args(["sync", "resolve", "--endpoint", &server.endpoint])
            .args(&invalid)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        retained(&tree);
    }
    retained(&tree);
    let unconfirmed = serde_json::json!({"recovery_id":RECOVERY_ID,"revision":selected["recovery_revision"],"target_identity":selected["target_identity"],"action":"discard","operation_id":"wire-unconfirmed"});
    server
        .request(
            "POST",
            "/_internal/sync/resolve",
            Some("all"),
            &serde_json::to_vec(&unconfirmed).unwrap(),
        )
        .error(400, "recovery_confirmation_required");
    retained(&tree);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/body", Some("all"), b"")
            .json(),
        winner
    );
    let outcome = server.cli_with_token("resolve", &exact, Some("all"), true);
    assert_eq!(outcome["completed"], true);
    assert_eq!(
        server.cli_with_token("resolve", &exact, Some("all"), true),
        outcome
    );
    let mut changed = exact.clone();
    changed[10] = "--confirm-backup-verified";
    assert_eq!(
        server.cli_with_token("resolve", &changed, Some("all"), false)["error"]["errors"][0]["reason"],
        "operation_id_reused"
    );
    assert!(
        !tree
            .0
            .join(".gcs-mimic-sync-work")
            .join(RECOVERY_ID)
            .exists()
    );
    settle(&server, Some("all"));
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/body", Some("all"), b"")
            .json(),
        winner
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/body")).unwrap(),
        b"API winner"
    );
    server.stop();
    let server = Server::start_with(&data, Some(&tree), &options);
    settle(&server, Some("all"));
    assert_eq!(
        server.cli_with_token("resolve", &exact, Some("all"), true),
        outcome
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/body", Some("all"), b"")
            .json(),
        winner
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/body?alt=media",
                Some("all"),
                b""
            )
            .body,
        b"API winner"
    );
    assert!(
        !tree
            .0
            .join(".gcs-mimic-sync-work")
            .join(RECOVERY_ID)
            .exists()
    );
}
