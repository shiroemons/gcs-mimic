use super::*;
use crate::{
    clock::SystemClock,
    storage::{
        Catalog,
        admin::{BucketName, Origin},
        test_fixtures::PERMISSIVE,
    },
    test_support::TestDir,
};

const DIGEST: RecordedEntry = RecordedEntry {
    digest: EntryDigest::new([7; 32]),
    members: MemberBits::new(0b101),
};

fn target(name: &str) -> InitTarget {
    InitTarget {
        name: name.into(),
        project: "project".into(),
    }
}

fn definition(name: &str) -> BucketDefinition {
    BucketDefinition::default_for(&BucketName::new(name).unwrap(), "project", &SystemClock).unwrap()
}

/// Creates `name` with [`DIGEST`] recorded, as a start of the initializer would.
async fn create(catalog: &Catalog, name: &str) {
    let definition = definition(name);
    catalog
        .admin(Origin::BucketInit)
        .initialize_buckets(vec![target(name)], move |observed| {
            assert!(observed[0].existing.is_none());
            let creation = Creation {
                definition,
                policy: None,
                entry: DIGEST,
            };
            Ok::<_, AdminError>((vec![Planned::Create(Box::new(creation))], ()))
        })
        .await
        .unwrap();
}

/// Runs one plan step for `name` that changes nothing but the recorded entry.
async fn record(catalog: &Catalog, name: &str, update: EntryUpdate) {
    catalog
        .admin(Origin::BucketInit)
        .initialize_buckets(vec![target(name)], move |_| {
            Ok::<_, AdminError>((vec![Planned::Record(update)], ()))
        })
        .await
        .unwrap();
}

/// The entry a start would observe for `name`, or `None` when the bucket is missing.
async fn observed_digest(catalog: &Catalog, name: &str) -> Option<Option<RecordedEntry>> {
    catalog
        .admin(Origin::BucketInit)
        .initialize_buckets(vec![target(name)], |observed| {
            let entry = observed
                .into_iter()
                .next()
                .and_then(|observed| observed.existing)
                .map(|stored| stored.entry);
            Ok::<_, AdminError>((vec![Planned::Keep], entry))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_created_bucket_records_its_entry_and_a_rejected_plan_changes_nothing() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    create(&catalog, "recorded-bucket").await;
    assert_eq!(
        observed_digest(&catalog, "recorded-bucket").await,
        Some(Some(DIGEST))
    );
    let definition = definition("refused-bucket");
    let refused = catalog
        .admin(Origin::BucketInit)
        .initialize_buckets(vec![target("refused-bucket")], move |_| {
            drop(definition);
            Err::<(Vec<Planned>, ()), _>(AdminError::Forbidden)
        })
        .await;
    assert_eq!(refused, Err(AdminError::Forbidden));
    assert_eq!(observed_digest(&catalog, "refused-bucket").await, None);
    catalog.close().await.unwrap();
    // The digest is durable catalog state, not memory of one start.
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        observed_digest(&catalog, "recorded-bucket").await,
        Some(Some(DIGEST))
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn deleting_or_soft_deleting_a_bucket_forgets_its_entry_and_restore_starts_without_one() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    create(&catalog, "deleted-bucket").await;
    catalog
        .delete_bucket("deleted-bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(observed_digest(&catalog, "deleted-bucket").await, None);
    create(&catalog, "deleted-bucket").await;
    assert_eq!(
        observed_digest(&catalog, "deleted-bucket").await,
        Some(Some(DIGEST))
    );

    catalog.set_default_soft_delete_retention(std::num::NonZeroU64::new(3600));
    create(&catalog, "retained-bucket").await;
    let generation = catalog
        .get_bucket("retained-bucket", PERMISSIVE)
        .await
        .unwrap()
        .unwrap()
        .generation;
    catalog
        .delete_bucket("retained-bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let rows: i64 = catalog
        .run(|connection| {
            sql::scalar(connection, "SELECT count(*) FROM bucket_file_entries", [])
                .map_err(CatalogError::from)
        })
        .await
        .unwrap();
    assert_eq!(rows, 1, "only the recreated bucket keeps an entry");
    catalog
        .restore_bucket("retained-bucket", generation, PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(
        observed_digest(&catalog, "retained-bucket").await,
        Some(None)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn record_replaces_the_entry_or_its_members_without_touching_the_bucket() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    create(&catalog, "kept-bucket").await;
    let before = catalog
        .get_bucket("kept-bucket", PERMISSIVE)
        .await
        .unwrap()
        .unwrap();
    let replaced = RecordedEntry {
        digest: EntryDigest::new([9; 32]),
        members: MemberBits::new(0b1),
    };
    record(&catalog, "kept-bucket", EntryUpdate::Matched(replaced)).await;
    assert_eq!(
        observed_digest(&catalog, "kept-bucket").await,
        Some(Some(replaced))
    );
    // Only the members move, so the recorded digest still reports the difference.
    let members = MemberBits::new(0b110);
    record(&catalog, "kept-bucket", EntryUpdate::Members(members)).await;
    assert_eq!(
        observed_digest(&catalog, "kept-bucket").await,
        Some(Some(RecordedEntry {
            digest: replaced.digest,
            members
        }))
    );
    assert_eq!(
        catalog
            .get_bucket("kept-bucket", PERMISSIVE)
            .await
            .unwrap()
            .unwrap(),
        before
    );
    // A bucket without a record gets none from a members-only update.
    catalog
        .insert_bucket(definition("api-bucket").into_record())
        .await
        .unwrap();
    record(&catalog, "api-bucket", EntryUpdate::Members(members)).await;
    assert_eq!(observed_digest(&catalog, "api-bucket").await, Some(None));
    catalog.close().await.unwrap();
}
