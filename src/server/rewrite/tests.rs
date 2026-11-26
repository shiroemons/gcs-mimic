use super::*;
use crate::config::CompatProfile;

#[tokio::test]
async fn a_revoked_json_destination_grant_prevents_session_registration() {
    use crate::storage::{
        fault::{Fault, Step},
        test_fixtures::{PERMISSIVE, body, bucket, pause_read, permissive_upload},
    };
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("test-bucket", "project"))
        .await
        .unwrap();
    catalog
        .put_object(
            permissive_upload("test-bucket", "source", 8),
            body(vec![bytes::Bytes::from_static(b"source")]),
        )
        .await
        .unwrap();
    let mut policy = catalog.get_policy("test-bucket", PERMISSIVE).await.unwrap();
    policy.bindings.push(crate::auth::Binding {
        role: "roles/storage.objectCreator".parse().unwrap(),
        members: vec!["allAuthenticatedUsers".parse().unwrap()],
    });
    catalog
        .set_policy("test-bucket", policy, PERMISSIVE)
        .await
        .unwrap();
    let identity = crate::auth::Identity::from_upload_snapshot(
        &serde_json::json!({"id":"rewriter","projects":{}}),
    )
    .unwrap();
    let job = RewriteJob {
        operation: crate::json::rewrite::route(
            &Method::POST,
            &"/storage/v1/b/test-bucket/o/source/rewriteTo/b/test-bucket/o/destination"
                .parse()
                .unwrap(),
            CompatProfile::Native,
        )
        .unwrap()
        .unwrap(),
        session: None,
        body: None,
        keys: CopyKeys::default(),
        source_access: BucketAccess::Permissive,
        destination_access: BucketAccess::Policy {
            identity: Some(identity),
            permission: auth::OBJECTS_CREATE,
            project: Some("project".into()),
        },
    };
    let (entered, release) = pause_read(&catalog, Step::DerivedDraftSync);
    let worker = tokio::spawn(execute(job, catalog.clone(), 8, "http://localhost".into()));
    entered.await.unwrap();
    let mut revoked = catalog.get_policy("test-bucket", PERMISSIVE).await.unwrap();
    revoked.bindings.clear();
    catalog
        .set_policy("test-bucket", revoked, PERMISSIVE)
        .await
        .unwrap();
    // A late publication denial would already have reached the registration commit.
    catalog.inject(
        Step::DerivedBeginCommit,
        0,
        Fault::Io(std::io::ErrorKind::Other),
    );
    release.send(()).unwrap();
    let error = worker.await.unwrap().err().unwrap();
    assert_eq!(
        (error.status, error.reason),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    assert_eq!(catalog.pending_faults(), 1);
    assert!(crate::storage::fault::hit(&dir.0, Step::DerivedBeginCommit).is_err());
    catalog.close().await.unwrap();
    let db = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let rows: i64 = db.query_row("SELECT (SELECT count(*) FROM rewrite_sessions) + (SELECT count(*) FROM derived_transfers) + (SELECT count(*) FROM derived_inputs)", [], |row| row.get(0)).unwrap();
    assert_eq!(rows, 0);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

/// A rewrite session stores the bound parameters as JSON, key order included.
#[test]
fn bound_parameters_keep_their_json_bytes() {
    let base = "/storage/v1/b/source-bucket/o/a/rewriteTo/b/destination-bucket/o/c";
    // Defaults the request left out follow its own parameters, in this order.
    for (query, bound) in [
        (
            "?sourceGeneration=5&ifGenerationMatch=007&ifSourceMetagenerationNotMatch=-0&fields=done&prettyPrint=False&projection=noAcl&destinationPredefinedAcl=private&maxBytesRewrittenPerCall=1048576&gcsmimicUnknown=1&key=k",
            r#"{"destinationPredefinedAcl":"private","fields":"done","ifGenerationMatch":"7","ifSourceMetagenerationNotMatch":"0","maxBytesRewrittenPerCall":"1048576","prettyPrint":"False","projection":"noAcl","sourceGeneration":"5","alt":"json"}"#,
        ),
        (
            "?maxBytesRewrittenPerCall=1048576",
            r#"{"maxBytesRewrittenPerCall":"1048576","projection":"noAcl","alt":"json","prettyPrint":"false"}"#,
        ),
    ] {
        let operation = crate::json::rewrite::route(
            &Method::POST,
            &format!("{base}{query}").parse().unwrap(),
            CompatProfile::Native,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::to_string(&bound_parameters(&operation.parameters)).unwrap(),
            bound
        );
    }
}
