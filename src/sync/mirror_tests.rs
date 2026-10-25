#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{
    clock::SystemClock,
    object::UploadChunk,
    storage::admin::{BucketDefinition, GenerationCondition, Publication, PublishMode},
    test_support::TestDir,
};
use bytes::Bytes;

async fn publish(catalog: &Catalog, bucket: &BucketName, body: &[u8]) -> ObjectRecord {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.try_send(Ok(UploadChunk::Data(Bytes::copy_from_slice(body))))
        .unwrap();
    tx.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .admin(Origin::SyncIngest)
        .publish_object(
            bucket,
            &ObjectName::new("object").unwrap(),
            Publication {
                content_type: "application/octet-stream".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            rx,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn export_concurrent_api_and_external_changes_never_strand_unpublished_operations() {
    for (phase, action) in [
        ("before-open", "overwrite"),
        ("before-open", "delete"),
        ("before-arm", "overwrite"),
        ("before-arm", "delete"),
        ("before-publish", "external-create"),
        ("before-durable", "external-edit"),
        ("before-durable", "overwrite"),
        ("before-durable", "delete"),
    ] {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        let admin = catalog.admin(Origin::SyncIngest);
        admin
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let record = publish(&catalog, &bucket, b"api").await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        catalog.set_change_journal_enabled(true).await.unwrap();
        let fence = catalog.read_changes(None, None, 1).await.unwrap().fence;
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        let path = tree.0.join("test-bucket/object");
        std::fs::create_dir(tree.0.join("test-bucket")).unwrap();
        let identical = action == "external-edit";
        if identical {
            std::fs::write(&path, b"api").unwrap();
        }
        let hook = std::sync::Arc::new(ExportHook {
            phase,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let name = ObjectName::new("object").unwrap();
        let output = (!identical).then(|| PathBuf::from("test-bucket/object"));
        let (result, ()) = tokio::join!(
            EXPORT_HOOK.scope(
                hook.clone(),
                export(&admin, &writeback, &bucket, &name, output, &record, fence)
            ),
            async {
                hook.entered.notified().await;
                match action {
                    "overwrite" => {
                        publish(&catalog, &bucket, b"new-api").await;
                    }
                    "delete" => {
                        admin
                            .delete_object(
                                &bucket,
                                &name,
                                GenerationCondition::Is(
                                    Generation::new(record.generation).unwrap(),
                                ),
                            )
                            .await
                            .unwrap();
                    }
                    _ => std::fs::write(&path, b"external").unwrap(),
                }
                hook.release.notify_one();
            }
        );
        assert!(!result.unwrap(), "{phase} {action}");
        assert_eq!(
            writeback.status().await.unwrap().stop,
            Stop::Ready,
            "{phase} {action}"
        );
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        if action.starts_with("external") {
            assert_eq!(std::fs::read(&path).unwrap(), b"external");
        }
        if action == "overwrite" {
            let (_, reader) = admin.open_object(&bucket, &name, None).await.unwrap();
            let mut file = reader.into_file().await;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
            assert_eq!(bytes, b"new-api");
        }
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        reconcile(&catalog, &root, &config).await.unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}
