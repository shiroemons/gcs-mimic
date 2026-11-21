//! Crashes and power losses while a live export installs its target, recovered through the
//! startup path and then checked with the wait that `sync wait` runs.

use super::*;
use crate::storage::fault::{Crash, Fault, Step};
use crate::sync::test_support::{operation_entries, restore_tree};
use crate::sync::{WaitReason, report::ending::WaitResult};
use std::os::unix::fs::PermissionsExt;

const BEFORE: &[u8] = b"before\0api";
const AFTER: &[u8] = b"after\0\xffapi";
const TARGET: &str = "test-bucket/object";

/// The API change whose export a crash interrupts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Create,
    Overwrite,
    Remove,
}

impl Change {
    const ALL: [Self; 3] = [Self::Create, Self::Overwrite, Self::Remove];

    /// The installation checkpoints the export passes. A removal copies no API body.
    fn steps(self) -> &'static [Step] {
        match self {
            Self::Create | Self::Overwrite => &[
                Step::SyncCopyDurable,
                Step::SyncCopyRenamed,
                Step::SyncCopyInstalled,
                Step::SyncCopyCommit,
            ],
            Self::Remove => &[
                Step::SyncCopyRenamed,
                Step::SyncCopyInstalled,
                Step::SyncCopyCommit,
            ],
        }
    }

    /// The target's bytes before the export.
    fn before(self) -> Option<&'static [u8]> {
        match self {
            Self::Create => None,
            Self::Overwrite | Self::Remove => Some(BEFORE),
        }
    }

    /// The target's bytes once the export is complete, which are also the private copy's.
    fn after(self) -> Option<&'static [u8]> {
        match self {
            Self::Create | Self::Overwrite => Some(AFTER),
            Self::Remove => None,
        }
    }

    /// The bytes in the private workspace entry and at the target that the snapshot at `step`
    /// holds. The parent syncs inside the rename's call make it durable before its checkpoint;
    /// when one of them failed, a power loss undoes the rename whole.
    fn snapshot(self, step: Step, crash: Crash) -> (Option<&'static [u8]>, Option<&'static [u8]>) {
        match (step, crash) {
            (Step::SyncCopyDurable, _) | (Step::SyncCopyFailed, Crash::PowerLoss) => {
                (self.after(), self.before())
            }
            // The rename stands, with the displaced or removed body not yet unlinked.
            (Step::SyncCopyRenamed | Step::SyncCopyFailed, _) => (self.before(), self.after()),
            (Step::SyncCopyInstalled | Step::SyncCopyCommit, _) => (None, self.after()),
            (step, _) => panic!("no installation checkpoint: {step:?}"),
        }
    }
}

/// Other bytes of the tracked body's size.
const OTHER: &[u8] = b"BEFORE\0API";

/// What reaches the tracked target after installation checked it and before the rename that
/// exchanges it with the copy or moves it out. A user's write is synced, so a power loss keeps it.
#[derive(Clone, Copy, Debug)]
enum Race {
    /// Its permissions change, which changes its ctime.
    Permissions,
    /// It comes back under another inode number with the same bytes and modification time.
    Renumbered,
    /// It holds other bytes of the same size under the same modification time.
    OtherBytes,
}

impl Race {
    const ALL: [Self; 3] = [Self::Permissions, Self::Renumbered, Self::OtherBytes];

    /// The bytes the rename moves into the private entry.
    fn displaced(self) -> &'static [u8] {
        match self {
            Self::Permissions | Self::Renumbered => BEFORE,
            Self::OtherBytes => OTHER,
        }
    }

    fn apply(self, tree: &Path) {
        let path = tree.join(TARGET);
        let before = fs::metadata(&path).unwrap();
        let modified = before.modified().unwrap();
        match self {
            Self::Permissions => {
                let mode = before.permissions().mode() ^ 0o040;
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            }
            Self::Renumbered => {
                let copy = path.with_file_name("object.renumbered");
                write_synced(tree, &copy, BEFORE, modified);
                fs::rename(&copy, &path).unwrap();
            }
            Self::OtherBytes => write_synced(tree, &path, OTHER, modified),
        }
    }
}

/// Writes `bytes` to `path` under the modification time `modified` and syncs the file, recording
/// that sync in the power-loss model of `tree`, which sees only the emulator's own syncs.
fn write_synced(tree: &Path, path: &Path, bytes: &[u8], modified: std::time::SystemTime) {
    fs::write(path, bytes).unwrap();
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(modified).unwrap();
    file.sync_all().unwrap();
    fault::file_synced(tree, &file.metadata().unwrap());
}

/// The bytes of each private workspace entry.
pub(super) fn workspace_bodies(tree: &Path) -> Vec<Vec<u8>> {
    operation_entries(&tree.join(mapping::WORKSPACE))
        .into_iter()
        .map(|entry| fs::read(entry).unwrap())
        .collect()
}

/// The bytes of every retained conflict in `data`.
pub(super) fn conflict_bodies(data: &Path) -> Vec<Vec<u8>> {
    match fs::read_dir(data.join("sync-conflicts")) {
        Ok(entries) => entries
            .map(|entry| fs::read(entry.unwrap().path()).unwrap())
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("{error}"),
    }
}

/// A mirror started the way the server starts it: write-back recovery, ingestion,
/// reconciliation and deletion observation.
pub(super) struct Started {
    pub(super) catalog: Catalog,
    pub(super) root: RootLease,
    config: Config,
    live: Initialized,
    stop: tokio::sync::watch::Sender<bool>,
}

impl Started {
    pub(super) async fn open(data: &Path, tree: &Path) -> Self {
        let catalog = Catalog::open(data).await.unwrap();
        Self::start(catalog, data, tree).await
    }

    pub(super) async fn start(catalog: Catalog, data: &Path, tree: &Path) -> Self {
        let config = Config {
            data_dir: data.to_owned(),
            sync_dir: Some(tree.to_owned()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let status = Lifecycle::default();
        let live = initialize_live(
            LiveRun {
                catalog: &catalog,
                root: &root,
                config: &config,
                status: &status,
                resume: &mut None,
            },
            &stopping,
        )
        .await
        .unwrap();
        Self {
            catalog,
            root,
            config,
            live,
            stop,
        }
    }

    /// Runs the wait `sync wait` sends, which also exports the API changes it replays.
    pub(super) async fn wait(&mut self) -> Result<WaitResult, SyncError> {
        let stopping = self.stop.subscribe();
        LiveControl {
            scope: LiveScope {
                catalog: &self.catalog,
                root: &self.root,
                config: &self.config,
                writeback: &self.live.writeback,
                stopping: &stopping,
                entries: &EntryReportSender::default(),
            },
            state: &mut self.live.state,
        }
        .wait(crate::test_support::hang_deadline())
        .await
    }

    pub(super) async fn close(self) {
        drop(self.live);
        drop(self.root);
        self.catalog.close().await.unwrap();
    }

    /// Starts the next run on the same catalog, with the write-back recovery, ingestion and
    /// reconciliation that the rescan resuming a paused mirror runs.
    pub(super) async fn rerun(self) -> Self {
        let tree = self.config.sync_dir.clone().unwrap();
        drop(self.live);
        drop(self.root);
        Self::start(self.catalog, &self.config.data_dir, &tree).await
    }
}

/// Makes the API change, returning the record it leaves, if any.
async fn change_api(
    catalog: &Catalog,
    bucket: &BucketName,
    change: Change,
    before: Option<&ObjectRecord>,
) -> Option<ObjectRecord> {
    match (change.after(), before) {
        (Some(after), _) => Some(publish_origin(catalog, bucket, after, Origin::Api).await),
        (None, Some(before)) => {
            catalog
                .admin(Origin::Api)
                .delete_object(
                    bucket,
                    &ObjectName::new("object").unwrap(),
                    GenerationCondition::Is(Generation::new(before.generation).unwrap()),
                )
                .await
                .unwrap();
            None
        }
        (None, None) => panic!("{change:?} has nothing to remove"),
    }
}

/// Checks that the API still holds `expected` and that the tree converged to it.
async fn assert_converged(started: &Started, bucket: &BucketName, expected: Option<&ObjectRecord>) {
    let name = ObjectName::new("object").unwrap();
    let tree = started.config.sync_dir.as_deref().unwrap();
    let current = started
        .catalog
        .admin(Origin::Api)
        .get_object(bucket, &name, None)
        .await;
    if let Some(record) = expected {
        assert_eq!(current.unwrap(), *record);
        assert_eq!(fs::read(tree.join(TARGET)).unwrap(), AFTER);
    } else {
        assert!(matches!(current, Err(AdminError::NotFound)));
        assert!(!tree.join(TARGET).exists());
    }
    assert_eq!(
        tracked_generation(&started.catalog, &started.root, bucket).await,
        expected.map(|record| record.generation)
    );
    assert!(workspace_bodies(tree).is_empty());
}

/// Fails the syncs inside a replacement's call of one directory until it drops, also when the test
/// panics, without touching the directories other tests running at the same time make fail.
struct FailingSync((u64, u64));

impl FailingSync {
    fn of(directory: &Path) -> Self {
        let identity = fsutil::stat_identity(&fs::File::open(directory).unwrap()).unwrap();
        let failing = (identity.device, identity.inode);
        fsutil::FAIL_REPLACE_SYNC.lock().unwrap().push(failing);
        Self(failing)
    }
}

impl Drop for FailingSync {
    fn drop(&mut self) {
        if let Ok(mut failing) = fsutil::FAIL_REPLACE_SYNC.lock() {
            failing.retain(|directory| *directory != self.0);
        }
    }
}

/// The tree and catalog left by a crash or power loss during an export, restored from the
/// snapshot it took.
struct Crashed {
    tree: TestDir,
    data: PathBuf,
    bucket: BucketName,
    /// The API record the change left.
    api: Option<ObjectRecord>,
    /// The directories `data` needs, removed when the test is done with them.
    _owned: Vec<TestDir>,
}

impl Crashed {
    /// Crashes or loses power at `step` of `change`'s export and restores the snapshot, checking
    /// that it holds exactly the state the checkpoint promises.
    async fn at(change: Change, step: Step, crash: Crash) -> Self {
        Self::run(change, step, crash, None, None).await
    }

    /// Like [`Self::at`] at [`Step::SyncCopyFailed`], after the export failed because the sync of
    /// `failing`, a parent of the rename, failed inside the rename's call.
    async fn failing(change: Change, failing: &str, crash: Crash) -> Self {
        Self::run(change, Step::SyncCopyFailed, crash, Some(failing), None).await
    }

    /// Like [`Self::at`] at [`Step::SyncCopyRenamed`], after `race` reached the target between
    /// installation's check of it and the rename.
    async fn raced(change: Change, crash: Crash, race: Race) -> Self {
        Self::run(change, Step::SyncCopyRenamed, crash, None, Some(race)).await
    }

    async fn run(
        change: Change,
        step: Step,
        crash: Crash,
        failing: Option<&str>,
        race: Option<Race>,
    ) -> Self {
        let case = format!("{change:?} {step:?} {crash:?} {failing:?} {race:?}");
        let (data, tree, catalog, bucket, _) = live_fixture().await;
        let before = match change.before() {
            Some(body) => Some(publish_origin(&catalog, &bucket, body, Origin::Api).await),
            None => None,
        };
        if failing.is_some() {
            fs::create_dir_all(tree.0.join("test-bucket")).unwrap();
        }
        let mut started = Started::start(catalog, &data.0, &tree.0).await;
        assert_eq!(
            fs::read(tree.0.join(TARGET)).ok().as_deref(),
            change.before()
        );
        let snapshots = TestDir::new();
        let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
        fault::arm(&tree.0, step, 0, crash.fault(snapshot_tree.clone()));
        started
            .catalog
            .inject(step, 0, Fault::Snapshot(snapshot_data.clone()));
        let failing_sync = failing.map(|failing| FailingSync::of(&tree.0.join(failing)));
        let held = race.map(|race| (race, install_race::Held::arm(&tree.0, Step::SyncCopyRename)));
        let api = change_api(&started.catalog, &bucket, change, before.as_ref()).await;
        let waited = match held {
            Some((race, held)) => {
                let (waited, ()) =
                    tokio::join!(started.wait(), held.change(|| race.apply(&tree.0)));
                waited
            }
            None => started.wait().await,
        };
        drop(failing_sync);
        match (failing, race) {
            (Some(_), _) => assert!(waited.is_err(), "{case}"),
            (None, Some(Race::OtherBytes)) => assert!(
                waited.is_err(),
                "{case}: {:?}",
                waited.as_ref().map(|waited| &waited.reason)
            ),
            (None, _) => assert_eq!(waited.unwrap().reason, WaitReason::Ready, "{case}"),
        }
        assert_eq!(fault::pending(&tree.0), 0, "{case}");
        assert_eq!(started.catalog.pending_faults(), 0, "{case}");
        started.close().await;

        restore_tree(&snapshot_tree, &tree.0, &[TARGET]);
        let (copy, target) = change.snapshot(step, crash);
        let copy = race.map_or(copy, |race| Some(race.displaced()));
        assert_eq!(
            workspace_bodies(&tree.0),
            copy.into_iter().map(<[u8]>::to_vec).collect::<Vec<_>>(),
            "{case}"
        );
        assert_eq!(
            fs::read(tree.0.join(TARGET)).ok().as_deref(),
            target,
            "{case}"
        );
        Self {
            tree,
            data: snapshot_data,
            bucket,
            api,
            _owned: vec![data, snapshots],
        }
    }

    /// Opens the restored catalog. A restore copies files, so the identities of tracked files
    /// whose bytes it kept are translated first.
    async fn open(&self) -> Catalog {
        let catalog = Catalog::open(&self.data).await.unwrap();
        crate::storage::sync_state::writeback::retrack_restored(&catalog, &self.tree.0)
            .await
            .unwrap();
        catalog
    }

    async fn start(&self, catalog: Catalog) -> Started {
        Started::start(catalog, &self.data, &self.tree.0).await
    }

    /// Restarts on the restored state and checks that `sync wait` succeeds without a conflict
    /// and that the tree converges to the API state.
    async fn recovers_cleanly(&self, case: &str) {
        let mut started = self.start(self.open().await).await;
        assert_eq!(
            started.wait().await.unwrap().reason,
            WaitReason::Ready,
            "{case}"
        );
        assert!(conflict_bodies(&self.data).is_empty(), "{case}");
        assert_converged(&started, &self.bucket, self.api.as_ref()).await;
        started.close().await;
    }
}

/// A crash or power loss at each installation checkpoint of a create, an overwrite and a
/// removal loses neither API nor external bytes and reports no conflict: the private entry holds
/// the API copy or the tracked body the rename displaced or removed, which recovery reclaims. The
/// restart recovers write-back, rescans and reconciles, `sync wait` succeeds, and the target
/// converges to the API state. The snapshot holds exactly the state the checkpoint promises, so
/// dropping the copy step's workspace sync, either parent sync inside the rename's call or the
/// workspace sync after the unlink fails this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installation_crashes_and_power_losses_recover_to_the_api_state() {
    for change in Change::ALL {
        for &step in change.steps() {
            for &crash in Crash::ALL {
                let case = format!("{change:?} {step:?} {crash:?}");
                Crashed::at(change, step, crash)
                    .await
                    .recovers_cleanly(&case)
                    .await;
            }
        }
    }
}

/// A parent sync that fails inside the rename's call leaves the rename volatile. A crash or power
/// loss right after the failed attempt, before the failure is recorded, recovers to the API state
/// on restart without a conflict: a power loss undoes the rename whole, so the private entry holds
/// the API copy again, and an abort keeps the rename with the displaced or removed body in the
/// entry. Recovery reclaims either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_after_a_parent_sync_failed_inside_the_rename_recovers_cleanly() {
    for change in Change::ALL {
        for failing in [mapping::WORKSPACE, "test-bucket"] {
            for &crash in Crash::ALL {
                let case = format!("{change:?} {failing} {crash:?}");
                Crashed::failing(change, failing, crash)
                    .await
                    .recovers_cleanly(&case)
                    .await;
            }
        }
    }
}

/// Recovery reclaims a private entry only when it holds exactly the API copy or the tracked body.
/// One changed byte in the API copy, in the body an overwrite displaced or in the body a removal
/// moved aside keeps it protected: the restart moves it into the conflict store and reports it,
/// and the target still converges to the API state. The API copy stays in an armed export's entry
/// when a power loss undoes a rename whose parent sync failed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_entry_one_byte_off_stays_protected_through_restart() {
    for change in Change::ALL {
        let case = format!("{change:?}");
        let crashed = match change {
            Change::Create => Crashed::failing(change, "test-bucket", Crash::PowerLoss).await,
            Change::Overwrite | Change::Remove => {
                Crashed::at(change, Step::SyncCopyRenamed, Crash::Abort).await
            }
        };
        let entry = operation_entries(&crashed.tree.0.join(mapping::WORKSPACE))
            .into_iter()
            .next()
            .unwrap();
        let mut edited = fs::read(&entry).unwrap();
        *edited.last_mut().unwrap() ^= 1;
        fs::write(&entry, &edited).unwrap();
        let mut started = crashed.start(crashed.open().await).await;
        assert_eq!(
            started.wait().await.unwrap().reason,
            WaitReason::FindingsPending,
            "{case}"
        );
        assert_eq!(conflict_bodies(&crashed.data), [edited], "{case}");
        assert_converged(&started, &crashed.bucket, crashed.api.as_ref()).await;
        started.close().await;
    }
}

/// Recovery reclaims a tracked body by the rule installation unlinks a displaced one by, which
/// compares the modification time as well as the bytes. The body an overwrite displaced or a
/// removal moved aside, with its bytes unchanged and only its modification time moved, stays
/// protected through the restart, as installation would have kept it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tracked_body_with_another_modification_time_stays_protected_through_restart() {
    for change in [Change::Overwrite, Change::Remove] {
        let case = format!("{change:?}");
        let crashed = Crashed::at(change, Step::SyncCopyRenamed, Crash::Abort).await;
        let entry = operation_entries(&crashed.tree.0.join(mapping::WORKSPACE))
            .into_iter()
            .next()
            .unwrap();
        let modified = fs::metadata(&entry).unwrap().modified().unwrap();
        fs::File::open(&entry)
            .unwrap()
            .set_modified(modified + Duration::from_secs(1))
            .unwrap();
        assert_eq!(fs::read(&entry).unwrap(), BEFORE, "{case}");
        let mut started = crashed.start(crashed.open().await).await;
        assert_eq!(
            started.wait().await.unwrap().reason,
            WaitReason::FindingsPending,
            "{case}"
        );
        assert_eq!(conflict_bodies(&crashed.data), [BEFORE], "{case}");
        assert_converged(&started, &crashed.bucket, crashed.api.as_ref()).await;
        started.close().await;
    }
}

/// A size and modification time that match the record do not make a tracked body reclaimable;
/// only its bytes do. The body an overwrite displaced or a removal moved aside, with its last byte
/// changed or one byte appended and its modification time then set back to the one the snapshot
/// restore kept, which is the recorded one, stays protected through the restart: the restart moves
/// it into the conflict store and reports it, and the target still converges to the API state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tracked_body_edited_under_its_recorded_mtime_stays_protected_through_restart() {
    #[derive(Clone, Copy, Debug)]
    enum Edit {
        LastByte,
        AppendedByte,
    }
    for change in [Change::Overwrite, Change::Remove] {
        for edit in [Edit::LastByte, Edit::AppendedByte] {
            let case = format!("{change:?} {edit:?}");
            let crashed = Crashed::at(change, Step::SyncCopyRenamed, Crash::Abort).await;
            let entry = operation_entries(&crashed.tree.0.join(mapping::WORKSPACE))
                .into_iter()
                .next()
                .unwrap();
            let recorded = fs::metadata(&entry).unwrap().modified().unwrap();
            let mut edited = fs::read(&entry).unwrap();
            assert_eq!(edited, BEFORE, "{case}");
            match edit {
                Edit::LastByte => *edited.last_mut().unwrap() ^= 1,
                Edit::AppendedByte => edited.push(0),
            }
            fs::write(&entry, &edited).unwrap();
            fs::File::open(&entry)
                .unwrap()
                .set_modified(recorded)
                .unwrap();
            assert_eq!(
                fs::metadata(&entry).unwrap().modified().unwrap(),
                recorded,
                "{case}"
            );
            let mut started = crashed.start(crashed.open().await).await;
            assert_eq!(
                started.wait().await.unwrap().reason,
                WaitReason::FindingsPending,
                "{case}"
            );
            assert_eq!(conflict_bodies(&crashed.data), [edited], "{case}");
            assert_converged(&started, &crashed.bucket, crashed.api.as_ref()).await;
            started.close().await;
        }
    }
}

/// A crash or power loss right after the rename of an overwrite or a removal whose target
/// changed between installation's check and the rename leaves the private entry holding what the
/// rename moved there, and restart recovery judges it by its bytes. A permission change or a new
/// inode number over the tracked bytes is reclaimed: the restart reports no conflict, `sync wait`
/// succeeds and the target converges. Other bytes of the same size and modification time stay
/// protected: the restart moves them into the conflict store and reports them, and the target
/// still converges to the API state.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crash_after_a_raced_rename_recovers_by_the_bytes_the_rename_moved() {
    for change in [Change::Overwrite, Change::Remove] {
        for race in Race::ALL {
            for &crash in Crash::ALL {
                let case = format!("{change:?} {race:?} {crash:?}");
                let crashed = Crashed::raced(change, crash, race).await;
                match race {
                    Race::Permissions | Race::Renumbered => crashed.recovers_cleanly(&case).await,
                    Race::OtherBytes => {
                        let mut started = crashed.start(crashed.open().await).await;
                        assert_eq!(
                            started.wait().await.unwrap().reason,
                            WaitReason::FindingsPending,
                            "{case}"
                        );
                        assert_eq!(conflict_bodies(&crashed.data), [OTHER], "{case}");
                        assert_converged(&started, &crashed.bucket, crashed.api.as_ref()).await;
                        started.close().await;
                    }
                }
            }
        }
    }
}

/// A crash or power loss while recovery reclaims a private entry, at each of its unlink, sync and
/// completion boundaries, leaves a state the next restart reclaims the same way: no conflict,
/// `sync wait` succeeds and the target converges. The two cases reclaim the API copy that a power
/// loss put back after a parent sync failed inside the rename's call, and the tracked body an
/// overwrite displaced.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn crashes_while_reclaiming_a_private_entry_recover_on_the_next_restart() {
    for (step, crash, failing) in [
        (Step::SyncCopyFailed, Crash::PowerLoss, Some("test-bucket")),
        (Step::SyncCopyRenamed, Crash::Abort, None),
    ] {
        for again_at in [
            Step::SyncRecoveryUnlink,
            Step::SyncRecoveryUnlinked,
            Step::SyncRecoveryParentSync,
            Step::SyncRecoveryParentDurable,
            Step::SyncRecoveryComplete,
            Step::SyncRecoveryCompleted,
        ] {
            for &again in Crash::ALL {
                let case = format!("{step:?} {crash:?} then {again_at:?} {again:?}");
                let crashed = Crashed::run(Change::Overwrite, step, crash, failing, None).await;
                let snapshots = TestDir::new();
                let (snapshot_data, snapshot_tree) =
                    (snapshots.0.join("data"), snapshots.0.join("tree"));
                let catalog = crashed.open().await;
                fault::arm(
                    &crashed.tree.0,
                    again_at,
                    0,
                    again.fault(snapshot_tree.clone()),
                );
                catalog.inject(again_at, 0, Fault::Snapshot(snapshot_data.clone()));
                let started = crashed.start(catalog).await;
                assert_eq!(fault::pending(&crashed.tree.0), 0, "{case}");
                assert_eq!(started.catalog.pending_faults(), 0, "{case}");
                started.close().await;

                restore_tree(&snapshot_tree, &crashed.tree.0, &[TARGET]);
                let restored = Crashed {
                    data: snapshot_data,
                    _owned: vec![snapshots],
                    ..crashed
                };
                restored.recovers_cleanly(&case).await;
            }
        }
    }
}

/// A parent directory sync that fails after the rename leaves the export displaced. A restart
/// recovers it without losing API or external bytes, and the target converges to the API state.
/// The failure protects the bytes the rename displaced or removed while the process runs, and
/// recovery never reclaims a protected entry, so they stay in the conflict store.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_parent_sync_failing_after_the_rename_recovers_on_restart() {
    for change in Change::ALL {
        for failing in [mapping::WORKSPACE, "test-bucket"] {
            let case = format!("{change:?} {failing}");
            let (data, tree, catalog, bucket, _) = live_fixture().await;
            let before = match change.before() {
                Some(body) => Some(publish_origin(&catalog, &bucket, body, Origin::Api).await),
                None => None,
            };
            fs::create_dir_all(tree.0.join("test-bucket")).unwrap();
            let mut started = Started::start(catalog, &data.0, &tree.0).await;
            let failing_sync = FailingSync::of(&tree.0.join(failing));
            let api = change_api(&started.catalog, &bucket, change, before.as_ref()).await;
            let failed = started.wait().await;
            drop(failing_sync);
            assert!(
                failed.is_err(),
                "{case}: {:?}",
                failed.map(|result| result.reason)
            );
            started.close().await;

            let mut started = Started::open(&data.0, &tree.0).await;
            let reason = started.wait().await.unwrap().reason;
            let displaced = change.before();
            assert_eq!(
                conflict_bodies(&data.0),
                displaced
                    .into_iter()
                    .map(<[u8]>::to_vec)
                    .collect::<Vec<_>>(),
                "{case}"
            );
            assert_eq!(
                reason,
                if displaced.is_some() {
                    WaitReason::FindingsPending
                } else {
                    WaitReason::Ready
                },
                "{case}"
            );
            assert_converged(&started, &bucket, api.as_ref()).await;
            started.close().await;
        }
    }
}
