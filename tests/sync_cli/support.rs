use super::*;
use std::fs;

pub(super) fn settle(server: &Server, token: Option<&str>) {
    server.cli_with_token("rescan", &[], token, true);
    assert_eq!(
        server.cli_with_token("wait", &["--timeout", "10s"], token, true)["completed"],
        true
    );
}

pub(super) fn scan_with_conflicts(server: &Server, token: Option<&str>) {
    server.cli_with_token("rescan", &[], token, true);
    let stopped = server.cli_with_token("wait", &["--timeout", "10s"], token, false);
    assert_eq!(stopped["completed"], false);
    assert_eq!(stopped["reason"], "FindingsPending");
}

pub(super) fn conflict_options<'a>(
    item: &'a Value,
    action: &'a str,
    generation: &'a str,
    operation: &'a str,
) -> Vec<&'a str> {
    vec![
        "--conflict",
        item["conflict_id"].as_str().unwrap(),
        "--revision",
        item["revision"].as_str().unwrap(),
        "--action",
        action,
        "--expected-generation",
        generation,
        "--operation-id",
        operation,
    ]
}

pub(super) async fn api_fixture(data: &TestDir, tree: &TestDir, names: &[&str]) {
    use gcs_mimic::{
        clock::SystemClock,
        storage::{
            Catalog,
            admin::{BucketDefinition, BucketName, ObjectName, Origin, Publication, PublishMode},
        },
    };
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let admin = catalog.admin(Origin::Api);
    admin
        .ensure_bucket(BucketDefinition::default_for(&bucket, "original", &SystemClock).unwrap())
        .await
        .unwrap();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let input = data.0.join("fixture-input");
    fs::write(&input, b"API winner").unwrap();
    for name in names {
        admin
            .publish_from_file(
                &bucket,
                &ObjectName::new(*name).unwrap(),
                Publication {
                    content_type: "application/octet-stream".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: None,
                    max_bytes: 4096,
                    mode: PublishMode::Replace,
                },
                fs::File::open(&input).unwrap(),
            )
            .await
            .unwrap();
        let external: &[u8] = if *name == "discard" {
            b""
        } else {
            b"external\0\xff"
        };
        fs::write(tree.0.join("test-bucket").join(name), external).unwrap();
    }
    drop(admin);
    let config = gcs_mimic::config::Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    let root = gcs_mimic::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    gcs_mimic::sync::reconcile(&catalog, &root, &config)
        .await
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}
