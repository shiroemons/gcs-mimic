//! What exporting an API deletion leaves of the directories around its target.

use super::*;

/// Exports `name` through every step and returns the record it tracks.
async fn exported(fixture: &Fixture, name: &str) -> crate::object::ObjectRecord {
    let record = fixture.publish(name, b"api").await;
    let operation = fixture.begin(name).await;
    let mut result = LiveResult::Pending;
    for _ in 0..4 {
        result = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    assert_eq!(result, LiveResult::Complete);
    record
}

/// Deletes `record` through the API and begins exporting that deletion.
async fn begin_removal(fixture: &Fixture, record: &crate::object::ObjectRecord) -> LiveExport {
    let name = ObjectName::new(record.name.clone()).unwrap();
    let generation = Generation::new(record.generation).unwrap();
    fixture
        .catalog
        .admin(Origin::Api)
        .delete_object(
            &fixture.bucket,
            &name,
            crate::storage::admin::GenerationCondition::Is(generation),
        )
        .await
        .unwrap();
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    let tracked = fixture
        .catalog
        .sync_state()
        .get_path(binding.clone(), format!("test-bucket/{}", record.name))
        .await
        .unwrap();
    assert!(tracked.is_some());
    fixture
        .writeback
        .begin_live_export(
            binding,
            name,
            generation,
            tracked,
            fixture.catalog.current_fence().await.unwrap(),
            LiveSource::Observe,
        )
        .await
        .unwrap()
}

/// A deletion whose target's directory someone removed completes without creating that directory
/// again: the target went with it, and a directory put back would be one nobody asked for.
#[tokio::test]
async fn a_removal_leaves_a_missing_directory_missing() {
    let fixture = Fixture::new().await;
    let record = exported(&fixture, "nested/object").await;
    let nested = fixture.tree.0.join("test-bucket/nested");
    fs::remove_dir_all(&nested).unwrap();
    let operation = begin_removal(&fixture, &record).await;
    let mut result = LiveResult::Pending;
    for _ in 0..4 {
        result = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    assert_eq!(result, LiveResult::Complete);
    assert!(!nested.exists());
    assert!(fixture.tree.0.join("test-bucket").is_dir());
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    assert!(
        fixture
            .catalog
            .sync_state()
            .get_path(binding, "test-bucket/nested/object".into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 0);
    drop(operation);
    fixture.close().await;
}
