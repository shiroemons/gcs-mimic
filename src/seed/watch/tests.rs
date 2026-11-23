//! Passes while the server runs, on a manual clock: a test changes the fixture tree or the catalog,
//! starts a pass through the board, and moves the clock past the wait that the second proof of a
//! deletion needs.

use super::*;
// Only a Unix walk proves a name absent, so the tests of deletions run there alone.
#[cfg(unix)]
use crate::object::MapPatch;
use crate::{
    bucket::{BucketAccess, BucketConditions},
    clock::ManualClock,
    object::{ObjectConditions, ObjectMetadataUpdate, ObjectRecord, ObjectTarget},
    storage::admin::{
        ObjectName,
        seed_watch::{LiveCounts, WaitError},
    },
    test_support::{TestDir, hang_deadline},
};
#[cfg(unix)]
use std::{collections::BTreeMap, num::NonZeroUsize};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

const PERMISSIVE: &BucketAccess = &BucketAccess::Permissive;

/// A seeded catalog whose passes run on a manual clock, over `bucket/a`, `bucket/b` and
/// `bucket/c`, each holding its own name.
struct Watched {
    dir: TestDir,
    seed: PathBuf,
    config: Config,
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the deletions, which a Unix walk alone proves, wait for a second proof"
        )
    )]
    clock: Arc<ManualClock>,
    catalog: Catalog,
}

async fn watched() -> Watched {
    watched_with(|_| {}).await
}

async fn watched_with(configure: impl FnOnce(&mut Config)) -> Watched {
    let dir = TestDir::new();
    let seed = dir.0.join("seed");
    std::fs::create_dir_all(seed.join("bucket")).unwrap();
    for name in ["a", "b", "c"] {
        std::fs::write(seed.join("bucket").join(name), name).unwrap();
    }
    let data = dir.0.join("data");
    std::fs::create_dir(&data).unwrap();
    let mut config = Config {
        data_dir: data,
        seed_dir: Some(seed.clone()),
        ..Config::default()
    };
    configure(&mut config);
    let clock = Arc::new(ManualClock::new(SystemTime::now()));
    let catalog = Catalog::open_with_clock(&config.data_dir, clock.clone())
        .await
        .unwrap();
    let shared: Arc<dyn Clock> = clock.clone();
    crate::seed::import(&catalog, &config, &shared)
        .await
        .unwrap();
    watch(&catalog, &config, shared);
    Watched {
        dir,
        seed,
        config,
        clock,
        catalog,
    }
}

fn names(bucket: &str, name: &str) -> (BucketName, ObjectName) {
    (
        BucketName::new(bucket).unwrap(),
        ObjectName::new(name).unwrap(),
    )
}

impl Watched {
    fn write(&self, relative: &str, body: &[u8]) {
        let path = self.seed.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// Saves `body` as an editor does: into a new file beside the old one, renamed over it.
    fn rename_save(&self, relative: &str, body: &[u8]) {
        let path = self.seed.join(relative);
        let saved = self.dir.0.join("saved");
        std::fs::write(&saved, body).unwrap();
        std::fs::rename(&saved, path).unwrap();
    }

    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "a removed file deletes an object only where a Unix walk proves it absent"
        )
    )]
    fn remove(&self, relative: &str) {
        let path = self.seed.join(relative);
        if path.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }

    async fn pass(&self) -> PassReport {
        self.catalog.seed_watch().next_pass().await
    }

    /// Moves the clock past the wait between the two proofs of a deletion, then waits for a
    /// pass that left no deletion waiting.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the deletions, which a Unix walk alone proves, wait for a second proof"
        )
    )]
    async fn confirm(&self) -> PassReport {
        self.clock.advance(Duration::from_secs(2));
        self.catalog
            .seed_watch()
            .wait(hang_deadline())
            .await
            .unwrap()
    }

    async fn live(&self, bucket: &str, name: &str) -> Option<ObjectRecord> {
        let (bucket, name) = names(bucket, name);
        match self
            .catalog
            .admin(Origin::Seed)
            .get_object(&bucket, &name, None)
            .await
        {
            Ok(record) => Some(record),
            Err(AdminError::NotFound) => None,
            Err(error) => panic!("{error}"),
        }
    }

    async fn generation(&self, bucket: &str, name: &str) -> i64 {
        self.live(bucket, name).await.unwrap().generation
    }

    async fn body(&self, bucket: &str, name: &str) -> Vec<u8> {
        let (bucket, name) = names(bucket, name);
        let (_, mut reader) = self
            .catalog
            .admin(Origin::Seed)
            .open_object(&bucket, &name, None)
            .await
            .unwrap();
        let mut body = Vec::new();
        while let Some(chunk) = reader.next_chunk().await.unwrap() {
            body.extend_from_slice(&chunk);
        }
        body
    }

    /// Writes `body` at `bucket/name` as an API client would, with what a replay would write
    /// besides the body.
    async fn api_write(&self, bucket: &str, name: &str, body: &[u8]) -> ObjectRecord {
        let path = self.dir.0.join("upload");
        std::fs::write(&path, body).unwrap();
        let (bucket, object) = names(bucket, name);
        self.catalog
            .admin(Origin::Seed)
            .publish_from_file(
                &bucket,
                &object,
                Publication {
                    content_type: self.config.seed_content_type.for_name(name).into(),
                    expected: ExpectedHashes::default(),
                    condition: None,
                    metadata: Default::default(),
                    max_bytes: self.config.max_object_bytes,
                    mode: PublishMode::Replace,
                },
                std::fs::File::open(&path).unwrap(),
            )
            .await
            .unwrap()
    }

    async fn api_update(&self, bucket: &str, name: &str, update: ObjectMetadataUpdate) {
        self.catalog
            .update_object_metadata(
                ObjectTarget::new(bucket, name, None),
                ObjectConditions::default(),
                update,
                |_| Ok(()),
                PERMISSIVE,
            )
            .await
            .unwrap();
    }

    async fn api_delete(&self, bucket: &str, name: &str) {
        self.catalog
            .delete_object(
                ObjectTarget::new(bucket, name, None),
                ObjectConditions::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
    }

    /// Deletes the seeded bucket and its objects as API requests would.
    async fn api_delete_bucket(&self) {
        for name in ["a", "b", "c"] {
            self.api_delete("bucket", name).await;
        }
        self.catalog
            .delete_bucket("bucket", BucketConditions::default(), PERMISSIVE)
            .await
            .unwrap();
    }

    async fn bucket_exists(&self, bucket: &str) -> bool {
        self.catalog
            .admin(Origin::Seed)
            .get_bucket(&BucketName::new(bucket).unwrap())
            .await
            .unwrap()
            .is_some()
    }

    async fn close(self) {
        self.catalog.close().await.unwrap();
    }
}

#[track_caller]
fn assert_clean(report: &PassReport) {
    assert!(
        report.complete && report.error.is_none() && report.problems.is_empty(),
        "{report:?}"
    );
}

#[tokio::test]
async fn a_pass_over_an_unchanged_tree_changes_nothing() {
    let watched = watched().await;
    let before = watched.generation("bucket", "a").await;
    let report = watched.pass().await;
    assert_clean(&report);
    assert_eq!(report.counts, LiveCounts::default());
    assert_eq!(watched.generation("bucket", "a").await, before);
    watched.close().await;
}

#[tokio::test]
async fn a_pass_publishes_new_and_changed_files_and_creates_the_bucket_of_a_new_directory() {
    let watched = watched().await;
    watched.write("bucket/a", b"changed");
    watched.write("bucket/new/deep", b"deep");
    watched.write("added/x", b"x");
    let report = watched.pass().await;
    assert_clean(&report);
    assert_eq!(report.counts.published, 3);
    assert_eq!(report.counts.buckets_created, 1);
    assert_eq!(watched.body("bucket", "a").await, b"changed");
    assert_eq!(watched.body("bucket", "new/deep").await, b"deep");
    assert_eq!(watched.body("added", "x").await, b"x");
    watched.close().await;
}

/// A deletion waits for a second proof a second after the first, and a file that returns with
/// the body seeding deleted restores the object.
#[cfg(unix)]
#[tokio::test]
async fn a_removed_file_deletes_its_object_after_a_second_proof_and_restores_it_on_return() {
    let watched = watched().await;
    watched.remove("bucket/b");
    let first = watched.pass().await;
    assert_eq!(first.counts.pending_deletions, 1);
    assert!(watched.live("bucket", "b").await.is_some());
    watched.confirm().await;
    assert!(watched.live("bucket", "b").await.is_none());
    watched.write("bucket/b", b"b");
    let report = watched.pass().await;
    assert_eq!(report.counts.published, 1, "{report:?}");
    assert_eq!(watched.body("bucket", "b").await, b"b");
    watched.close().await;
}

/// An editor that unlinks a file and renames another into its place leaves the name empty for a
/// moment; a pass in between only starts the wait, and the file found again cancels it.
#[cfg(unix)]
#[tokio::test]
async fn a_file_missing_for_one_pass_keeps_its_object() {
    let watched = watched().await;
    let before = watched.generation("bucket", "b").await;
    watched.remove("bucket/b");
    assert_eq!(watched.pass().await.counts.pending_deletions, 1);
    watched.write("bucket/b", b"b");
    watched.pass().await;
    watched.confirm().await;
    assert_eq!(watched.generation("bucket", "b").await, before);
    watched.close().await;
}

#[tokio::test]
async fn an_api_overwrite_stays_until_the_file_changes_even_through_a_rename_save() {
    let watched = watched().await;
    let written = watched.api_write("bucket", "a", b"api").await;
    watched.pass().await;
    assert_eq!(watched.generation("bucket", "a").await, written.generation);
    watched.rename_save("bucket/a", b"a");
    watched.pass().await;
    assert_eq!(watched.generation("bucket", "a").await, written.generation);
    watched.rename_save("bucket/a", b"edited");
    let report = watched.pass().await;
    assert_eq!(report.counts.published, 1);
    assert_eq!(watched.body("bucket", "a").await, b"edited");
    watched.close().await;
}

#[tokio::test]
async fn an_api_deletion_stays_until_the_file_changes() {
    let watched = watched().await;
    watched.api_delete("bucket", "a").await;
    watched.pass().await;
    assert!(watched.live("bucket", "a").await.is_none());
    watched.write("bucket/a", b"changed");
    watched.pass().await;
    assert_eq!(watched.body("bucket", "a").await, b"changed");
    watched.close().await;
}

/// A pass that fails between two proofs of absence, here because the seed directory went away
/// for it, breaks the run: the next proof starts the wait again, however long ago the first was.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_pass_between_two_proofs_starts_the_wait_again() {
    let watched = watched().await;
    watched.remove("bucket/b");
    assert_eq!(watched.pass().await.counts.pending_deletions, 1);
    let aside = watched.dir.0.join("aside");
    std::fs::rename(&watched.seed, &aside).unwrap();
    let failed = watched.pass().await;
    assert!(failed.error.is_some(), "{failed:?}");
    std::fs::rename(&aside, &watched.seed).unwrap();
    watched.clock.advance(Duration::from_secs(2));
    let report = watched.pass().await;
    assert_eq!(
        (report.counts.deleted, report.counts.pending_deletions),
        (0, 1),
        "{report:?}"
    );
    assert!(watched.live("bucket", "b").await.is_some());
    watched.confirm().await;
    assert!(watched.live("bucket", "b").await.is_none());
    watched.close().await;
}

/// The name of a removed file whose object an API write replaced keeps a tombstone, which the
/// status counts, and the object stays.
#[cfg(unix)]
#[tokio::test]
async fn a_removed_file_keeps_the_object_an_api_write_made() {
    let watched = watched().await;
    let written = watched.api_write("bucket", "c", b"api").await;
    watched.remove("bucket/c");
    watched.pass().await;
    let report = watched.confirm().await;
    assert_eq!(report.counts.tombstones, 1);
    assert_eq!(report.counts.deleted, 0);
    assert_eq!(watched.generation("bucket", "c").await, written.generation);
    watched.close().await;
}

/// A metadata edit between the two proofs is a version seeding does not own, so the deletion's
/// conditions keep the object.
#[cfg(unix)]
#[tokio::test]
async fn an_api_edit_between_the_two_proofs_keeps_the_object() {
    let watched = watched().await;
    watched.remove("bucket/b");
    assert_eq!(watched.pass().await.counts.pending_deletions, 1);
    watched
        .api_update(
            "bucket",
            "b",
            ObjectMetadataUpdate {
                metadata: MapPatch::Merge(BTreeMap::from([("owner".into(), Some("api".into()))])),
                ..Default::default()
            },
        )
        .await;
    let report = watched.confirm().await;
    assert_eq!(report.counts.deleted, 0);
    assert_eq!(watched.live("bucket", "b").await.unwrap().metageneration, 2);
    watched.close().await;
}

/// A held object is reported and kept, and once released it stays until the file changes again.
#[tokio::test]
async fn a_protected_object_is_reported_and_kept_until_its_file_changes_again() {
    let watched = watched().await;
    let hold = |held| ObjectMetadataUpdate {
        temporary_hold: Some(held),
        ..Default::default()
    };
    watched.api_update("bucket", "a", hold(true)).await;
    watched.write("bucket/a", b"changed");
    let report = watched.pass().await;
    assert_eq!(report.problems.listed().len(), 1, "{report:?}");
    assert_eq!(
        report.problems.listed()[0].path.as_deref(),
        Some("bucket/a")
    );
    assert_eq!(watched.body("bucket", "a").await, b"a");
    watched.api_update("bucket", "a", hold(false)).await;
    assert_clean(&watched.pass().await);
    assert_eq!(watched.body("bucket", "a").await, b"a");
    watched.write("bucket/a", b"again");
    watched.pass().await;
    assert_eq!(watched.body("bucket", "a").await, b"again");
    watched.close().await;
}

/// A walk stopped by a limit proves nothing absent: the removed file's object stays.
#[cfg(unix)]
#[tokio::test]
async fn an_incomplete_walk_deletes_nothing_and_reports_why() {
    let watched = watched_with(|config| config.max_seed_entries = NonZeroUsize::new(5)).await;
    for name in ["d", "e", "f"] {
        watched.write(&format!("bucket/{name}"), name.as_bytes());
    }
    watched.remove("bucket/b");
    let report = watched.pass().await;
    assert!(!report.complete);
    assert_eq!(report.counts.pending_deletions, 0);
    let reasons = report.problems.listed();
    assert!(
        reasons
            .iter()
            .any(|problem| problem.path.is_none() && problem.reason.contains("max_seed_entries")),
        "{report:?}"
    );
    let report = watched.confirm().await;
    assert!(!report.complete);
    assert!(watched.live("bucket", "b").await.is_some());
    watched.close().await;
}

/// A removed bucket directory deletes the objects seeding owns there, never the bucket.
#[cfg(unix)]
#[tokio::test]
async fn a_removed_directory_deletes_its_objects_and_keeps_its_bucket() {
    let watched = watched().await;
    watched.remove("bucket");
    assert_eq!(watched.pass().await.counts.pending_deletions, 3);
    let report = watched.confirm().await;
    assert_clean(&report);
    for name in ["a", "b", "c"] {
        assert!(watched.live("bucket", name).await.is_none(), "{name}");
    }
    assert!(watched.bucket_exists("bucket").await);
    watched.close().await;
}

/// A bucket an API request deleted is not created again while its directory stays.
#[tokio::test]
async fn a_bucket_the_api_deleted_stays_deleted_while_its_directory_stays() {
    let watched = watched().await;
    watched.api_delete_bucket().await;
    watched.write("bucket/d", b"d");
    let report = watched.pass().await;
    assert_clean(&report);
    assert_eq!(report.counts.buckets_kept_deleted, 1);
    assert!(!watched.bucket_exists("bucket").await);
    watched.close().await;
}

/// A directory away for one pass keeps the bucket an API request deleted deleted; one removed
/// for two passes a second apart and added back gets it again, as a new directory does.
#[cfg(unix)]
#[tokio::test]
async fn a_directory_removed_and_added_back_gets_the_bucket_the_api_deleted() {
    let watched = watched().await;
    watched.api_delete_bucket().await;
    let aside = watched.dir.0.join("aside");
    let directory = watched.seed.join("bucket");
    std::fs::rename(&directory, &aside).unwrap();
    assert_eq!(watched.pass().await.counts.pending_directories, 1);
    std::fs::rename(&aside, &directory).unwrap();
    watched.clock.advance(Duration::from_secs(2));
    let report = watched.pass().await;
    assert_eq!(report.counts.buckets_kept_deleted, 1, "{report:?}");
    assert!(!watched.bucket_exists("bucket").await);

    std::fs::rename(&directory, &aside).unwrap();
    assert_eq!(watched.pass().await.counts.pending_directories, 1);
    assert_clean(&watched.confirm().await);
    std::fs::rename(&aside, &directory).unwrap();
    watched.write("bucket/d", b"d");
    let report = watched.pass().await;
    assert_eq!(report.counts.buckets_created, 1);
    assert_eq!(report.counts.published, 4);
    assert_eq!(watched.body("bucket", "d").await, b"d");
    watched.close().await;
}

/// A directory a pass cannot list proves nothing gone: once it is readable again, a bucket an
/// API request deleted stays deleted, and one it created again keeps the objects it holds.
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_directory_keeps_the_bucket_the_api_deleted_or_created_again() {
    use std::os::unix::fs::PermissionsExt;
    for recreated in [false, true] {
        let watched = watched().await;
        watched.api_delete_bucket().await;
        let api = if recreated {
            watched
                .catalog
                .admin(Origin::Seed)
                .ensure_bucket(
                    BucketDefinition::default_for(
                        &BucketName::new("bucket").unwrap(),
                        &watched.config.seed_project,
                        &crate::clock::SystemClock,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            Some(watched.api_write("bucket", "a", b"api").await)
        } else {
            None
        };
        let directory = watched.seed.join("bucket");
        let mode =
            |bits| std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(bits));
        mode(0o000).unwrap();
        if std::fs::read_dir(&directory).is_ok() {
            mode(0o755).unwrap();
            eprintln!(
                "SKIP: a mode 000 directory stays listable (for example when running as root)"
            );
            return;
        }
        let report = watched.pass().await;
        let confirmed = watched.confirm().await;
        mode(0o755).unwrap();
        assert!(
            report
                .problems
                .listed()
                .iter()
                .any(|problem| problem.path.as_deref() == Some("bucket")),
            "{report:?}"
        );
        assert_eq!(confirmed.counts.pending_directories, 0, "{confirmed:?}");
        let report = watched.pass().await;
        assert_eq!(report.counts.buckets_created, 0, "{report:?}");
        assert_eq!(report.counts.published, 0, "{report:?}");
        match api {
            Some(written) => {
                assert_eq!(watched.generation("bucket", "a").await, written.generation);
                assert_eq!(watched.body("bucket", "a").await, b"api");
            }
            None => assert!(!watched.bucket_exists("bucket").await),
        }
        watched.close().await;
    }
}

/// A removed file whose name the lookup cannot tell about, as below a directory a walk may list
/// but not search here and as on every walk on Windows, keeps its object, and every pass reports
/// the name while seeding owns that object. Once an API write replaced or deleted it, the name is
/// a tombstone and the passes are clean, before and after the lookup proves the file gone.
#[cfg(unix)]
#[tokio::test]
async fn a_name_the_lookup_cannot_tell_about_is_reported_only_while_seeding_owns_its_object() {
    use std::os::unix::fs::PermissionsExt;
    let watched = watched().await;
    watched.write("bucket/sub/replaced", b"replaced");
    watched.write("bucket/sub/deleted", b"deleted");
    watched.pass().await;
    watched.remove("bucket/sub/replaced");
    watched.remove("bucket/sub/deleted");
    let sub = watched.seed.join("bucket/sub");
    let mode = |bits| std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(bits));
    mode(0o444).unwrap();
    if std::fs::metadata(sub.join("deleted"))
        .map(|_| ())
        .map_err(|error| error.kind())
        != Err(std::io::ErrorKind::PermissionDenied)
    {
        mode(0o755).unwrap();
        eprintln!(
            "SKIP: a directory without search permission stays searchable (for example when running as root)"
        );
        return;
    }
    let reported = watched.confirm().await;
    let mut paths: Vec<_> = reported
        .problems
        .listed()
        .iter()
        .map(|problem| problem.path.clone())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        [
            Some("bucket/sub/deleted".into()),
            Some("bucket/sub/replaced".into())
        ],
        "{reported:?}"
    );
    assert_eq!(watched.body("bucket", "sub/deleted").await, b"deleted");
    assert_eq!(watched.body("bucket", "sub/replaced").await, b"replaced");

    let written = watched.api_write("bucket", "sub/replaced", b"api").await;
    watched.api_delete("bucket", "sub/deleted").await;
    for _ in 0..2 {
        let report = watched.pass().await;
        assert_clean(&report);
        assert_eq!(report.counts.tombstones, 2, "{report:?}");
    }
    mode(0o755).unwrap();
    let report = watched.confirm().await;
    assert_clean(&report);
    assert_eq!(report.counts.tombstones, 2, "{report:?}");
    assert_eq!(
        watched.generation("bucket", "sub/replaced").await,
        written.generation
    );
    assert!(watched.live("bucket", "sub/deleted").await.is_none());
    watched.close().await;
}

/// Like a startup replay, a pass emits no bucket notification for what it publishes or deletes,
/// while an API request in the same bucket still emits one.
#[cfg(unix)]
#[tokio::test]
async fn a_pass_emits_no_bucket_notifications() {
    let watched = watched().await;
    watched
        .catalog
        .insert_notification(
            "bucket",
            crate::notification::NotificationConfig {
                topic: "//pubsub.googleapis.com/projects/p/topics/topic"
                    .parse()
                    .unwrap(),
                payload_format: crate::notification::PayloadFormat::JsonApiV1,
                event_types: crate::notification::EventTypes::default(),
                custom_attributes: std::collections::BTreeMap::new(),
                object_name_prefix: String::new(),
            },
            PERMISSIVE,
        )
        .await
        .unwrap();
    watched.write("bucket/a", b"changed");
    watched.write("bucket/new", b"new");
    watched.remove("bucket/b");
    assert_eq!(watched.pass().await.counts.published, 2);
    assert_eq!(watched.confirm().await.counts.deleted, 1);
    assert!(watched.catalog.next_notification().await.unwrap().is_none());
    watched.api_delete("bucket", "c").await;
    assert!(watched.catalog.next_notification().await.unwrap().is_some());
    watched.close().await;
}

/// An entry seeding cannot use is reported, and the object of the file it replaced stays.
#[cfg(unix)]
#[tokio::test]
async fn an_entry_seeding_cannot_use_is_reported_and_its_object_kept() {
    let watched = watched().await;
    let outside = watched.dir.0.join("outside");
    std::fs::write(&outside, b"outside").unwrap();
    watched.remove("bucket/a");
    std::os::unix::fs::symlink(&outside, watched.seed.join("bucket/a")).unwrap();
    let report = watched.pass().await;
    assert_eq!(
        report.problems.listed(),
        [crate::storage::admin::seed_watch::Problem {
            path: Some("bucket/a".into()),
            reason: SeedError::UnsupportedFile.to_string(),
        }]
    );
    watched.confirm().await;
    assert_eq!(watched.body("bucket", "a").await, b"a");
    watched.close().await;
}

#[tokio::test]
async fn the_status_reports_the_last_pass_and_a_closed_catalog_ends_the_passes() {
    let watched = watched().await;
    let status = |watched: &Watched| serde_json::to_value(watched.catalog.seed_watch().status());
    let before = status(&watched).unwrap();
    assert_eq!(before["enabled"], true);
    assert_eq!(before["intervalMs"], 1000);
    assert!(before["lastPass"].is_null());
    watched.pass().await;
    let after = status(&watched).unwrap();
    assert_eq!(after["lastPass"]["complete"], true);
    assert_eq!(after["lastPass"]["clean"], true);
    assert_eq!(after["lastPass"]["published"], 0);
    let watch = watched.catalog.clone();
    watched.close().await;
    assert_eq!(
        watch.seed_watch().wait(hang_deadline()).await.unwrap_err(),
        WaitError::Closed
    );
}

#[tokio::test]
async fn no_pass_runs_with_a_zero_interval() {
    let watched = watched_with(|config| config.seed_watch_interval = None).await;
    let status = serde_json::to_value(watched.catalog.seed_watch().status()).unwrap();
    assert_eq!(status["enabled"], false);
    assert_eq!(
        watched
            .catalog
            .seed_watch()
            .wait(hang_deadline())
            .await
            .unwrap_err(),
        WaitError::NotConfigured
    );
    watched.close().await;
}
