//! Exporting an object whose target name a directory holds.

use super::*;
use crate::storage::catalog::fault::{Fault, Step};
use crate::storage::sync_state::observation::PathStop;

/// The directory at the target of the object `a`.
const DIRECTORY: &str = "test-bucket/a";

/// Advances `operation` until it ends.
async fn finish(fixture: &Fixture, operation: &LiveExport) -> Result<LiveResult, Error> {
    for _ in 0..8 {
        let result = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await?;
        if result != LiveResult::Pending {
            return Ok(result);
        }
    }
    panic!("the export did not end")
}

/// Publishes `a`, makes the directory at its target and begins exporting it.
async fn begin_over_directory(fixture: &Fixture) -> LiveExport {
    fixture.publish("a", b"api").await;
    fs::create_dir_all(fixture.tree.0.join(DIRECTORY)).unwrap();
    fixture.begin("a").await
}

/// The names in `directory`, sorted.
fn names(directory: &std::path::Path) -> Vec<String> {
    let mut names = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Checks that the export of `a` ended as an occupied directory that left the directory holding
/// `holds`, and that it finished, with no private copy left behind.
async fn assert_kept(fixture: &Fixture, result: LiveResult, holds: &[&str]) {
    assert_eq!(
        result,
        LiveResult::Collision(mapping::Reason::DirectoryOccupied)
    );
    assert!(fixture.tree.0.join(DIRECTORY).is_dir());
    assert_eq!(names(&fixture.tree.0.join(DIRECTORY)), holds);
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 0);
    let copies = names(&fixture.tree.0.join(mapping::WORKSPACE))
        .into_iter()
        .filter(|name| name != crate::sync::naming::PROBE_DIRECTORY)
        .count();
    assert_eq!(copies, 0);
}

/// Checks that the export of `a` completed into the place of the removed directory.
async fn assert_exported(fixture: &Fixture, result: LiveResult) {
    assert_eq!(result, LiveResult::Complete);
    assert_eq!(fs::read(fixture.tree.0.join(DIRECTORY)).unwrap(), b"api");
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
            .get_path(binding, DIRECTORY.into())
            .await
            .unwrap()
            .is_some()
    );
}

/// An empty directory that nothing in the catalog refers to gives way to the export, which
/// removes it and installs the object in its place.
#[tokio::test]
async fn an_empty_directory_gives_way_to_the_export() {
    let fixture = Fixture::new().await;
    let operation = begin_over_directory(&fixture).await;
    let result = finish(&fixture, &operation).await.unwrap();
    assert_exported(&fixture, result).await;
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 0);
    drop(operation);
    fixture.close().await;
}

/// What a test puts in the directory at the target.
#[derive(Clone, Copy, Debug)]
enum Held {
    /// A regular file.
    File(&'static str),
    /// An empty directory.
    Directory(&'static str),
    /// A symlink to a regular file outside the directory.
    Symlink(&'static str),
}

impl Held {
    fn name(self) -> &'static str {
        match self {
            Self::File(name) | Self::Directory(name) | Self::Symlink(name) => name,
        }
    }

    /// Puts this entry into `directory`, a symlink pointing at `outside`.
    fn put(self, directory: &std::path::Path, outside: &std::path::Path) {
        let path = directory.join(self.name());
        match self {
            Self::File(_) => fs::write(path, b"external").unwrap(),
            Self::Directory(_) => fs::create_dir(path).unwrap(),
            Self::Symlink(_) => std::os::unix::fs::symlink(outside, path).unwrap(),
        }
    }

    /// Checks that this entry is still in `directory` as it was put there.
    fn assert_kept(self, directory: &std::path::Path) {
        let path = directory.join(self.name());
        match self {
            Self::File(_) => assert_eq!(fs::read(path).unwrap(), b"external"),
            Self::Directory(_) => assert!(path.is_dir()),
            Self::Symlink(_) => assert!(path.is_symlink()),
        }
    }
}

/// A directory that holds anything but a regular `.DS_Store` stays, with everything it holds: a
/// file, an editor's swap file or AppleDouble file that scans ignore, an empty directory, a
/// `.DS_Store` beside another file, a `.DS_Store` that is a directory or a symlink, and more
/// entries than a removal lists. Nothing in it is removed, not even its `.DS_Store`. On macOS,
/// `rmdir` itself removes a directory that holds only an AppleDouble file of a name it does not
/// hold, so the `._a` case fails unless the removal lists the directory first.
#[tokio::test]
async fn a_directory_that_holds_anything_stays_a_collision() {
    let crowded: Vec<Held> = std::iter::once(Held::File(".DS_Store"))
        .chain(
            ["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8"]
                .into_iter()
                .map(Held::File),
        )
        .collect();
    let cases: [&[Held]; 8] = [
        &[Held::File("file")],
        &[Held::File(".a.swp")],
        &[Held::File("._a")],
        &[Held::Directory("inner")],
        &[Held::File(".DS_Store"), Held::File("file")],
        &[Held::Directory(".DS_Store")],
        &[Held::Symlink(".DS_Store")],
        &crowded,
    ];
    for held in cases {
        let fixture = Fixture::new().await;
        let operation = begin_over_directory(&fixture).await;
        let directory = fixture.tree.0.join(DIRECTORY);
        let outside = fixture.data.0.join("outside");
        fs::write(&outside, b"outside").unwrap();
        for entry in held {
            entry.put(&directory, &outside);
        }
        let result = finish(&fixture, &operation).await.unwrap();
        let mut names = held.iter().map(|entry| entry.name()).collect::<Vec<_>>();
        names.sort_unstable();
        assert_kept(&fixture, result, &names).await;
        for entry in held {
            entry.assert_kept(&directory);
        }
        assert_eq!(fs::read(&outside).unwrap(), b"outside", "{held:?}");
        drop(operation);
        fixture.close().await;
    }
}

/// A directory that holds only a regular `.DS_Store`, which Finder writes into every directory it
/// shows, gives way like an empty one: the export removes that file and the directory and
/// installs the object in their place.
#[tokio::test]
async fn a_directory_that_holds_only_ds_store_gives_way() {
    let fixture = Fixture::new().await;
    let operation = begin_over_directory(&fixture).await;
    fs::write(fixture.tree.0.join(DIRECTORY).join(".DS_Store"), b"finder").unwrap();
    let result = finish(&fixture, &operation).await.unwrap();
    assert_exported(&fixture, result).await;
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 0);
    drop(operation);
    fixture.close().await;
}

/// What the catalog records about a path at or below the target.
#[derive(Clone, Copy, Debug)]
enum Reference {
    /// A tracked file below the target, whose file someone removed.
    Tracked,
    /// A path stop on the target's own object.
    StopAt,
    /// A path stop on an object below the target.
    StopBelow,
    /// Another unfinished operation below the target. Write-back runs one export at a time, so
    /// the row is written directly, as one a later change to that rule could leave.
    Pending,
    /// A retained conflict of an object below the target.
    Conflict,
}

impl Reference {
    const ALL: [Self; 5] = [
        Self::Tracked,
        Self::StopAt,
        Self::StopBelow,
        Self::Pending,
        Self::Conflict,
    ];

    /// Records this reference for `name`.
    async fn record(self, fixture: &Fixture, name: &str) {
        let binding = fixture
            .catalog
            .sync_state()
            .binding(&fixture.bucket)
            .await
            .unwrap();
        let object = ObjectName::new(name).unwrap();
        let root = fixture.writeback.identities().1;
        let sql = match self {
            Self::Tracked => {
                fixture.publish(name, b"tracked").await;
                let operation = fixture.begin(name).await;
                assert_eq!(
                    finish(fixture, &operation).await.unwrap(),
                    LiveResult::Complete
                );
                fs::remove_file(fixture.tree.0.join("test-bucket").join(name)).unwrap();
                return;
            }
            Self::StopAt | Self::StopBelow => {
                fixture
                    .root
                    .record_path_stop(binding, object, PathStop::SourceChanging)
                    .await
                    .unwrap();
                return;
            }
            Self::Pending => {
                "INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) SELECT lower(hex(randomblob(16))),?1,'owner',bucket,incarnation,?2,'test-bucket/'||?2,1,0,0,1,0,0,zeroblob(8),0,zeroblob(16),0 FROM bucket_incarnations WHERE bucket='test-bucket'"
            }
            Self::Conflict => {
                "INSERT INTO sync_conflicts(id,root_id,revision,owner,bucket,incarnation,name,generation,state,size,source) SELECT lower(hex(randomblob(16))),?1,lower(hex(randomblob(16))),'owner',bucket,incarnation,?2,1,1,0,x'' FROM bucket_incarnations WHERE bucket='test-bucket'"
            }
        };
        let inserted = fixture
            .catalog
            .run_admin_with_error(move |connection| -> Result<usize, Error> {
                Ok(connection
                    .prepare_cached(sql)?
                    .execute(params![root, object])?)
            })
            .await
            .unwrap();
        assert_eq!(inserted, 1, "{self:?}");
    }
}

/// Each kind of catalog reference at or below the target keeps an empty directory there, on its
/// own: removing it would lose the place of a file the catalog still expects or an object it
/// still reports.
#[tokio::test]
async fn each_catalog_reference_keeps_an_empty_directory() {
    for reference in Reference::ALL {
        let fixture = Fixture::new().await;
        let name = match reference {
            Reference::StopAt => "a",
            Reference::Tracked
            | Reference::StopBelow
            | Reference::Pending
            | Reference::Conflict => "a/x",
        };
        reference.record(&fixture, name).await;
        let operation = begin_over_directory(&fixture).await;
        let result = finish(&fixture, &operation).await.unwrap();
        assert_kept(&fixture, result, &[]).await;
        drop(operation);
        fixture.close().await;
    }
}

/// References to names that only start with the target's name, such as `a.txt`, `a-b`, `a0` and
/// `ab/x`, are not below it and keep nothing.
#[tokio::test]
async fn references_beside_the_target_keep_nothing() {
    let fixture = Fixture::new().await;
    Reference::Tracked.record(&fixture, "a-b").await;
    Reference::StopBelow.record(&fixture, "a0").await;
    Reference::Pending.record(&fixture, "a.txt").await;
    Reference::Conflict.record(&fixture, "ab/x").await;
    let operation = begin_over_directory(&fixture).await;
    let result = finish(&fixture, &operation).await.unwrap();
    assert_exported(&fixture, result).await;
    drop(operation);
    fixture.close().await;
}

/// A removal the filesystem refuses, because the directory holds an entry, may not be removed or
/// is in use, keeps the directory as a collision without failing the export. Any other error
/// fails the attempt with its own cause, not one about the directory still at the target, and a
/// retry removes the directory and completes.
#[tokio::test]
async fn a_refused_removal_is_a_collision_and_a_failed_one_is_retried() {
    for kind in [
        io::ErrorKind::DirectoryNotEmpty,
        io::ErrorKind::AlreadyExists,
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::ResourceBusy,
    ] {
        let fixture = Fixture::new().await;
        let operation = begin_over_directory(&fixture).await;
        fixture
            .catalog
            .inject(Step::SyncEmptyDirectoryRemove, 0, Fault::Io(kind));
        let result = finish(&fixture, &operation).await.unwrap();
        assert_kept(&fixture, result, &[]).await;
        assert_eq!(fixture.catalog.pending_faults(), 0, "{kind:?}");
        drop(operation);
        fixture.close().await;
    }
    let fixture = Fixture::new().await;
    let operation = begin_over_directory(&fixture).await;
    fixture.catalog.inject(
        Step::SyncEmptyDirectoryRemove,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    let failed = finish(&fixture, &operation).await;
    let Err(Error::Admin(AdminError::Io(cause))) = &failed else {
        panic!("{failed:?}")
    };
    assert_eq!(cause.kind(), io::ErrorKind::Other);
    assert!(fixture.tree.0.join(DIRECTORY).is_dir());
    let result = finish(&fixture, &operation).await.unwrap();
    assert_exported(&fixture, result).await;
    drop(operation);
    fixture.close().await;
}

/// A sync of the parent that fails after the removal fails the attempt; the retry finds the name
/// free and installs the object there.
#[tokio::test]
async fn a_failed_parent_sync_after_the_removal_is_retried() {
    let fixture = Fixture::new().await;
    let operation = begin_over_directory(&fixture).await;
    inject_failure(fixture.writeback.identities().1, "directory-parent-sync", 1);
    assert!(finish(&fixture, &operation).await.is_err());
    assert!(!fixture.tree.0.join(DIRECTORY).exists());
    let result = finish(&fixture, &operation).await.unwrap();
    assert_exported(&fixture, result).await;
    drop(operation);
    fixture.close().await;
}

/// Advances `operation` until it ends, running `change` on the target's path while the attempt
/// that removes the directory there waits at `step`: just before the removal, or just after it.
async fn race_removal(
    fixture: &Fixture,
    operation: &LiveExport,
    step: Step,
    change: impl FnOnce(&std::path::Path),
) -> Result<LiveResult, Error> {
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    fixture.catalog.inject(
        step,
        0,
        Fault::Block {
            entered: Arc::new(std::sync::Mutex::new(Some(entered))),
            release: Arc::new(std::sync::Mutex::new(blocked)),
        },
    );
    let writeback = fixture.writeback.clone();
    let selected = operation.clone();
    let work = crate::task::spawn_supervised("empty-directory-race-test", async move {
        for _ in 0..8 {
            let result = writeback.advance_live_export(selected.clone()).await?;
            if result != LiveResult::Pending {
                return Ok(result);
            }
        }
        panic!("the export did not end")
    });
    started.await.unwrap();
    change(&fixture.tree.0.join(DIRECTORY));
    release.send(()).unwrap();
    work.await.unwrap()
}

/// An entry that arrives after every check and before the removal keeps the directory: the
/// removal itself is the last check, and it removes only an empty directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_entry_that_arrives_before_the_removal_keeps_the_directory() {
    let fixture = Fixture::new().await;
    let operation = begin_over_directory(&fixture).await;
    let late = fixture.tree.0.join(DIRECTORY).join("late");
    let result = race_removal(
        &fixture,
        &operation,
        Step::SyncEmptyDirectoryRemove,
        |directory| fs::write(directory.join("late"), b"late").unwrap(),
    )
    .await;
    assert_kept(&fixture, result.unwrap(), &["late"]).await;
    assert_eq!(fs::read(&late).unwrap(), b"late");
    drop(operation);
    fixture.close().await;
}

/// A directory someone removes after the proof read it counts as removed, and the export
/// installs the object in its place. A file put at the name after the removal found it gone
/// stays, with its bytes: installation never replaces an entry at a name it found free, so that
/// attempt fails instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_removed_before_the_removal_counts_as_gone() {
    let fixture = Fixture::new().await;
    let operation = begin_over_directory(&fixture).await;
    let result = race_removal(
        &fixture,
        &operation,
        Step::SyncEmptyDirectoryRemove,
        |directory| fs::remove_dir(directory).unwrap(),
    )
    .await;
    assert_exported(&fixture, result.unwrap()).await;
    assert_eq!(fixture.catalog.pending_faults(), 0);
    drop(operation);
    fixture.close().await;

    // The removal reports the directory gone, and a file takes the name before the rename.
    let fixture = Fixture::new().await;
    let operation = begin_over_directory(&fixture).await;
    fixture.catalog.inject(
        Step::SyncEmptyDirectoryRemove,
        0,
        Fault::Io(io::ErrorKind::NotFound),
    );
    let result = race_removal(
        &fixture,
        &operation,
        Step::SyncEmptyDirectoryRemoved,
        |directory| {
            fs::remove_dir(directory).unwrap();
            fs::write(directory, b"user").unwrap();
        },
    )
    .await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(fs::read(fixture.tree.0.join(DIRECTORY)).unwrap(), b"user");
    drop(operation);
    fixture.close().await;
}

/// Whether the filesystem that holds the test directories opens a name under another letter
/// case.
fn test_filesystem_folds_case() -> bool {
    let probe = TestDir::new();
    fs::create_dir(probe.0.join("fold")).unwrap();
    probe.0.join("FOLD").is_dir()
}

/// Takes the entries of a directory as spelled the way they were opened while it lives.
struct UncheckedSpelling((u64, u64));

impl UncheckedSpelling {
    fn of(directory: &std::path::Path) -> Self {
        let identity = fsutil::stat_identity(&fs::File::open(directory).unwrap()).unwrap();
        let unchecked = (identity.device, identity.inode);
        fsutil::UNCHECKED_SPELLING.lock().unwrap().push(unchecked);
        Self(unchecked)
    }
}

impl Drop for UncheckedSpelling {
    fn drop(&mut self) {
        if let Ok(mut unchecked) = fsutil::UNCHECKED_SPELLING.lock() {
            unchecked.retain(|directory| *directory != self.0);
        }
    }
}

/// On a root that folds letter case, an empty directory whose parent lists it under the target's
/// exact spelling still gives way.
#[tokio::test]
async fn a_folding_root_removes_a_directory_listed_under_the_exact_spelling() {
    let fixture = Fixture::with_naming(crate::sync::naming::RootNaming::folding()).await;
    let operation = begin_over_directory(&fixture).await;
    let result = finish(&fixture, &operation).await.unwrap();
    assert_exported(&fixture, result).await;
    drop(operation);
    fixture.close().await;
}

/// On a root that folds letter case, a directory reached under another spelling is never removed,
/// even where the filesystem reports the spelling as exact, as a Linux mount that folds case
/// without the casefold flag does: `photos/`, emptied by deleting its tracked `a.jpg` on the
/// host, stays when `Photos` is created through the API, so that deletion still reaches the API
/// instead of stopping at a file where its parent was. Only a test filesystem that folds case
/// can answer for another spelling; elsewhere the test has nothing to show.
#[tokio::test]
async fn a_folding_root_keeps_a_directory_whose_spelling_it_cannot_list() {
    if !test_filesystem_folds_case() {
        return;
    }
    let fixture = Fixture::with_naming(crate::sync::naming::RootNaming::folding()).await;
    Reference::Tracked.record(&fixture, "photos/a.jpg").await;
    let bucket = fixture.tree.0.join("test-bucket");
    let _unchecked = UncheckedSpelling::of(&bucket);
    fixture.publish("Photos", b"api").await;
    let operation = fixture.begin("Photos").await;
    let result = finish(&fixture, &operation).await.unwrap();
    assert_eq!(result, LiveResult::Collision(mapping::Reason::Collision));
    assert_eq!(names(&bucket), ["photos"]);
    assert!(bucket.join("photos").is_dir());
    drop(operation);
    fixture.close().await;
}
