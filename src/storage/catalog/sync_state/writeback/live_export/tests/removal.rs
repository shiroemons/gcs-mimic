//! What exporting an API deletion leaves of the directories around its target.

use super::*;

/// Exports `name` through every step and returns the record it tracks.
async fn exported(fixture: &Fixture, name: &str) -> crate::object::ObjectRecord {
    let record = fixture.publish(name, b"api").await;
    let operation = fixture.begin(name).await;
    let mut result = InstallOutcome::Pending;
    for _ in 0..4 {
        result = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    assert_eq!(result, InstallOutcome::Complete);
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
    let mut result = InstallOutcome::Pending;
    for _ in 0..4 {
        result = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    assert_eq!(result, InstallOutcome::Complete);
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

/// A symlink that takes a tracked target after the removal checked it and before the rename moves
/// the target out is never moved: the rename first opens what it moves, without following a
/// symlink, to sync it, and fails there. The symlink stays at the target, the private entry stays
/// empty, and the root stops for recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_symlink_that_took_a_removed_target_before_the_rename_stays_at_the_target() {
    let fixture = Fixture::new().await;
    let record = exported(&fixture, "object").await;
    let operation = begin_removal(&fixture, &record).await;
    let result = super::exchange::race_exchange(&fixture, &operation, |path| {
        fs::remove_file(path).unwrap();
        std::os::unix::fs::symlink("elsewhere", path).unwrap();
    })
    .await;
    assert!(
        matches!(&result, Err(Error::Admin(AdminError::Io(error)))
            if error.kind() == io::Error::from_raw_os_error(libc::ELOOP).kind()),
        "{result:?}"
    );
    assert_eq!(
        fs::read_link(fixture.tree.0.join("test-bucket/object")).unwrap(),
        std::path::Path::new("elsewhere")
    );
    assert!(
        !super::exchange::private_entry(&fixture, &operation)
            .await
            .exists()
    );
    assert_eq!(
        fixture.writeback.status().await.unwrap().stop(),
        Stop::RecoveryRequired
    );
    drop(operation);
    fixture.close().await;
}

/// A directory with a user's file in it or a FIFO that takes a tracked target after the removal
/// checked it and before the rename is never moved either: the rename opens what it moves first,
/// and that is no regular file. The entry stays whole at the target, the private entry stays
/// empty, and the root stops for recovery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_or_fifo_that_took_a_removed_target_before_the_rename_stays_at_the_target() {
    use super::exchange::Taker;
    for taker in [Taker::Directory, Taker::Fifo] {
        let fixture = Fixture::new().await;
        let record = exported(&fixture, "object").await;
        let operation = begin_removal(&fixture, &record).await;
        let result =
            super::exchange::race_exchange(&fixture, &operation, |path| taker.apply(path)).await;
        assert!(
            matches!(&result, Err(Error::Admin(AdminError::Io(error)))
                if error.kind() == io::ErrorKind::InvalidInput),
            "{taker:?}: {result:?}"
        );
        let target = fixture.tree.0.join("test-bucket/object");
        match taker {
            Taker::Directory => assert_eq!(fs::read(target.join("inside")).unwrap(), b"user"),
            Taker::Fifo => assert!(std::os::unix::fs::FileTypeExt::is_fifo(
                &fs::symlink_metadata(&target).unwrap().file_type()
            )),
            Taker::LongerFile => unreachable!("only entries that are no regular file"),
        }
        assert!(
            !super::exchange::private_entry(&fixture, &operation)
                .await
                .exists(),
            "{taker:?}"
        );
        assert_eq!(
            fixture.writeback.status().await.unwrap().stop(),
            Stop::RecoveryRequired,
            "{taker:?}"
        );
        drop(operation);
        fixture.close().await;
    }
}
