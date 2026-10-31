use super::*;

fn target(fixture: &Fixture) -> PathBuf {
    let directory = fixture.tree.0.join("test-bucket");
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("object.txt");
    fs::write(&path, b"external\0\xff").unwrap();
    path
}

#[tokio::test]
async fn capture_path_reuses_verified_bytes_after_only_the_api_generation_changes() {
    let fixture = Fixture::new().await;
    let path = target(&fixture);
    let first = fixture
        .lease
        .capture_path(fixture.request().await)
        .await
        .unwrap();
    let mut request = fixture.request().await;
    let next = fixture
        .catalog
        .admin(Origin::Api)
        .publish_from_file(
            &fixture.bucket,
            &request.name,
            Publication {
                content_type: "text/custom".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            File::open(fixture.data.0.join("input")).unwrap(),
        )
        .await
        .unwrap();
    request.expected_generation = Some(Generation::new(next.generation).unwrap());
    let reused = fixture.lease.capture_path(request.clone()).await.unwrap();
    assert_eq!(first.id(), reused.id());
    assert_eq!(fixture.page().await.usage.entries, 1);
    assert_eq!(fixture.page().await.usage.retained_bytes, 10);
    assert_eq!(fs::read(path).unwrap(), b"external\0\xff");
    assert_eq!(reused.expected_generation, request.expected_generation);
    let id = first.id.clone();
    fixture
        .lease
        .conflict_db(move |connection| {
            let generation: i64 = connection
                .prepare_cached("SELECT generation FROM sync_conflicts WHERE id=?1")?
                .query_row([id], |row| row.get(0))?;
            assert_eq!(generation, first.expected_generation.unwrap().get());
            Ok(())
        })
        .await
        .unwrap();
    let proof = reused.clone();
    let root = fixture.lease.status().root_id.as_bytes();
    fixture
        .lease
        .conflict_db(move |connection| {
            let tx = connection.transaction()?;
            proof.validate(
                &tx,
                root,
                &request.binding,
                &request.name,
                request.expected_generation,
            )?;
            Ok(())
        })
        .await
        .unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn capture_path_never_reuses_changed_sources_or_missing_owned_copies() {
    let fixture = Fixture::new().await;
    let path = target(&fixture);
    let first = fixture
        .lease
        .capture_path(fixture.request().await)
        .await
        .unwrap();
    fs::write(&path, b"different!").unwrap();
    let changed = fixture
        .lease
        .capture_path(fixture.request().await)
        .await
        .unwrap();
    assert_ne!(first.id(), changed.id());
    fs::remove_file(fixture.data.0.join("sync-conflicts").join(changed.id())).unwrap();
    let repaired = fixture
        .lease
        .capture_path(fixture.request().await)
        .await
        .unwrap();
    assert_ne!(changed.id(), repaired.id());
    let again = fixture
        .lease
        .capture_path(fixture.request().await)
        .await
        .unwrap();
    assert_eq!(repaired.id(), again.id());
    let replacement = fixture.tree.0.join("replacement");
    fs::write(&replacement, b"different!").unwrap();
    fs::rename(replacement, path).unwrap();
    let different_inode = fixture
        .lease
        .capture_path(fixture.request().await)
        .await
        .unwrap();
    assert_ne!(repaired.id(), different_inode.id());
    assert_eq!(fixture.page().await.usage.entries, 4);
    fixture.close().await;
}

#[tokio::test]
async fn direct_capture_keeps_multiple_independently_requested_copies() {
    let fixture = Fixture::new().await;
    let first = fixture.capture().await;
    let second = fixture.capture().await;
    assert_ne!(first.id(), second.id());
    assert_eq!(fixture.page().await.usage.entries, 2);
    fixture.close().await;
}
