use super::*;
use crate::test_support::TestDir;
use std::io::Write;

#[test]
fn overlapping_power_loss_arms_keep_their_own_baselines_and_all_seam_updates() {
    let data = TestDir::new();
    let snapshots = TestDir::new();
    let baseline = data.0.join("baseline");
    let cut = data.0.join("cut");
    let removed = data.0.join("removed");
    let original = data.0.join("original");
    let renamed_path = data.0.join("renamed");
    let published = data.0.join("published");
    fs::create_dir(&published).unwrap();
    fs::write(&baseline, b"baseline").unwrap();
    fs::write(&cut, b"old tail").unwrap();
    fs::write(&removed, b"retained unlink").unwrap();
    fs::write(&original, b"renamed bytes").unwrap();
    let early = snapshots.0.join("early");
    let later = snapshots.0.join("later");
    arm(
        &data.0,
        Step::BeforeCommit,
        1,
        Fault::PowerLoss(early.clone()),
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&baseline)
        .unwrap()
        .write_all(b"tail")
        .unwrap();
    arm(
        &data.0,
        Step::AfterCommit,
        0,
        Fault::PowerLoss(later.clone()),
    );

    let mut file = fs::OpenOptions::new().write(true).open(&cut).unwrap();
    file.set_len(2).unwrap();
    truncated(&data.0, &file.metadata().unwrap());
    file.write_all(b"new unsynced tail").unwrap();
    fs::rename(&original, &renamed_path).unwrap();
    renamed(&original, &renamed_path);
    let retained = fs::File::open(&removed).unwrap();
    unlinked(&removed, &retained).unwrap();
    fs::remove_file(&removed).unwrap();
    let volatile = data.0.join("volatile");
    fs::write(&volatile, b"volatile entry").unwrap();
    file_created(&volatile);
    let directory = data.0.join("volatile-directory");
    fs::create_dir(&directory).unwrap();
    dir_created(&directory);
    fs::write(directory.join("child"), b"child").unwrap();
    let durable = published.join("blob");
    let mut blob = fs::File::create(&durable).unwrap();
    file_created(&durable);
    blob.write_all(b"durable bytes").unwrap();
    blob.sync_all().unwrap();
    file_synced(&data.0, &blob.metadata().unwrap());
    fs::File::open(&published).unwrap().sync_all().unwrap();
    dir_synced(&published);

    hit(&data.0, Step::AfterCommit).unwrap();
    hit(&data.0, Step::BeforeCommit).unwrap();
    assert!(!early.exists());
    hit(&data.0, Step::BeforeCommit).unwrap();
    assert_eq!(pending(&data.0), 0);
    for (snapshot, length) in [(&early, 8), (&later, 12)] {
        assert_eq!(
            fs::metadata(snapshot.join("baseline")).unwrap().len(),
            length
        );
        assert_eq!(fs::metadata(snapshot.join("cut")).unwrap().len(), 2);
        assert_eq!(
            fs::read(snapshot.join("removed")).unwrap(),
            b"retained unlink"
        );
        assert_eq!(
            fs::read(snapshot.join("original")).unwrap(),
            b"renamed bytes"
        );
        assert!(!snapshot.join("renamed").exists());
        assert!(!snapshot.join("volatile").exists());
        assert!(!snapshot.join("volatile-directory").exists());
        assert_eq!(
            fs::read(snapshot.join("published/blob")).unwrap(),
            b"durable bytes"
        );
    }
}

/// An exchange and a move between two directories stay volatile until both directories are
/// synced; a power loss before then undoes each whole.
#[test]
fn exchanges_and_moves_between_directories_need_both_directory_syncs() {
    let data = TestDir::new();
    let snapshots = TestDir::new();
    let left = data.0.join("left");
    let right = data.0.join("right");
    fs::create_dir(&left).unwrap();
    fs::create_dir(&right).unwrap();
    fs::write(left.join("swapped"), b"left bytes").unwrap();
    fs::write(right.join("swapped"), b"right bytes").unwrap();
    fs::write(left.join("moving"), b"moving bytes").unwrap();
    let unsynced = snapshots.0.join("unsynced");
    let one = snapshots.0.join("one");
    let both = snapshots.0.join("both");
    for (step, snapshot) in [
        (Step::BeforeCommit, &unsynced),
        (Step::AfterCommit, &one),
        (Step::PublishUpload, &both),
    ] {
        arm(&data.0, step, 0, Fault::PowerLoss(snapshot.clone()));
    }
    crate::fsutil::exchange_paths(&left.join("swapped"), &right.join("swapped")).unwrap();
    exchanged(&left.join("swapped"), &right.join("swapped"));
    crate::fsutil::rename_noreplace(&left.join("moving"), &right.join("moving")).unwrap();
    moved(&left.join("moving"), &right.join("moving"));
    hit(&data.0, Step::BeforeCommit).unwrap();
    dir_synced(&left);
    hit(&data.0, Step::AfterCommit).unwrap();
    dir_synced(&right);
    hit(&data.0, Step::PublishUpload).unwrap();
    assert_eq!(pending(&data.0), 0);
    for snapshot in [&unsynced, &one] {
        assert_eq!(
            fs::read(snapshot.join("left/swapped")).unwrap(),
            b"left bytes"
        );
        assert_eq!(
            fs::read(snapshot.join("right/swapped")).unwrap(),
            b"right bytes"
        );
        assert_eq!(
            fs::read(snapshot.join("left/moving")).unwrap(),
            b"moving bytes"
        );
        assert!(!snapshot.join("right/moving").exists());
    }
    assert_eq!(fs::read(both.join("left/swapped")).unwrap(), b"right bytes");
    assert_eq!(fs::read(both.join("right/swapped")).unwrap(), b"left bytes");
    assert_eq!(
        fs::read(both.join("right/moving")).unwrap(),
        b"moving bytes"
    );
    assert!(!both.join("left/moving").exists());
}

/// A volatile exchange whose name vanished cannot be undone; the snapshot fails instead of
/// keeping a state no power loss leaves.
#[test]
#[should_panic(expected = "cannot undo the volatile exchange")]
fn undoing_an_exchange_with_a_missing_name_fails_the_snapshot() {
    let data = TestDir::new();
    let snapshot = TestDir::new();
    fs::write(data.0.join("left"), b"left").unwrap();
    fs::write(data.0.join("right"), b"right").unwrap();
    arm(
        &data.0,
        Step::BeforeCommit,
        0,
        Fault::PowerLoss(snapshot.0.join("power-loss")),
    );
    crate::fsutil::exchange_paths(&data.0.join("left"), &data.0.join("right")).unwrap();
    exchanged(&data.0.join("left"), &data.0.join("right"));
    fs::remove_file(data.0.join("right")).unwrap();
    let _ = hit(&data.0, Step::BeforeCommit);
}

/// A volatile move whose old name was taken again cannot be undone without replacing that file.
#[test]
#[should_panic(expected = "cannot undo the volatile move")]
fn undoing_a_move_onto_a_taken_name_fails_the_snapshot() {
    let data = TestDir::new();
    let snapshot = TestDir::new();
    fs::create_dir(data.0.join("from")).unwrap();
    fs::create_dir(data.0.join("to")).unwrap();
    fs::write(data.0.join("from/body"), b"moved").unwrap();
    arm(
        &data.0,
        Step::BeforeCommit,
        0,
        Fault::PowerLoss(snapshot.0.join("power-loss")),
    );
    crate::fsutil::rename_noreplace(&data.0.join("from/body"), &data.0.join("to/body")).unwrap();
    moved(&data.0.join("from/body"), &data.0.join("to/body"));
    fs::write(data.0.join("from/body"), b"later").unwrap();
    let _ = hit(&data.0, Step::BeforeCommit);
}

/// A directory removal stays volatile until its parent is synced. A power loss before then brings
/// the empty directory back, and one that also undoes a later move into its name takes the file
/// back out first, newest first.
#[test]
fn a_directory_removal_needs_its_parent_sync() {
    let data = TestDir::new();
    let snapshots = TestDir::new();
    let parent = data.0.join("parent");
    let workspace = data.0.join("workspace");
    fs::create_dir_all(parent.join("removed")).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("copy"), b"installed").unwrap();
    let unsynced = snapshots.0.join("unsynced");
    let synced = snapshots.0.join("synced");
    for (step, snapshot) in [
        (Step::BeforeCommit, &unsynced),
        (Step::AfterCommit, &synced),
    ] {
        arm(&data.0, step, 0, Fault::PowerLoss(snapshot.clone()));
    }
    fs::remove_dir(parent.join("removed")).unwrap();
    dir_removed(&parent.join("removed"));
    crate::fsutil::rename_noreplace(&workspace.join("copy"), &parent.join("removed")).unwrap();
    moved(&workspace.join("copy"), &parent.join("removed"));
    hit(&data.0, Step::BeforeCommit).unwrap();
    dir_synced(&parent);
    dir_synced(&workspace);
    hit(&data.0, Step::AfterCommit).unwrap();
    assert_eq!(pending(&data.0), 0);
    let restored = unsynced.join("parent/removed");
    assert!(restored.is_dir());
    assert_eq!(fs::read_dir(&restored).unwrap().count(), 0);
    assert_eq!(
        fs::read(unsynced.join("workspace/copy")).unwrap(),
        b"installed"
    );
    assert_eq!(
        fs::read(synced.join("parent/removed")).unwrap(),
        b"installed"
    );
    assert!(!synced.join("workspace/copy").exists());
}

/// A volatile directory removal whose name was taken again cannot be undone without replacing
/// that entry; the snapshot fails instead of keeping a state no power loss leaves.
#[test]
#[should_panic(expected = "cannot undo the volatile removal of directory")]
fn undoing_a_directory_removal_onto_a_taken_name_fails_the_snapshot() {
    let data = TestDir::new();
    let snapshot = TestDir::new();
    fs::create_dir(data.0.join("removed")).unwrap();
    arm(
        &data.0,
        Step::BeforeCommit,
        0,
        Fault::PowerLoss(snapshot.0.join("power-loss")),
    );
    fs::remove_dir(data.0.join("removed")).unwrap();
    dir_removed(&data.0.join("removed"));
    fs::write(data.0.join("removed"), b"later").unwrap();
    let _ = hit(&data.0, Step::BeforeCommit);
}

#[test]
#[should_panic(expected = "BeforeCommit holds no transaction to roll back")]
fn a_rollback_armed_where_no_transaction_is_open_panics() {
    let data = TestDir::new();
    arm(&data.0, Step::BeforeCommit, 0, Fault::RollBack);
    let _ = hit(&data.0, Step::BeforeCommit);
}

#[test]
fn a_rollback_ends_the_open_transaction_and_fails_with_an_io_error() {
    let data = TestDir::new();
    let mut connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    let transaction = connection.transaction().unwrap();
    arm(&data.0, Step::PublishUpload, 0, Fault::RollBack);
    let failure = hit_transaction(&transaction, Step::PublishUpload);
    assert!(
        matches!(
            failure,
            Err(Injected::Sqlite(rusqlite::Error::SqliteFailure(ref error, _)))
                if error.extended_code == rusqlite::ffi::SQLITE_IOERR
        ),
        "{failure:?}"
    );
    assert!(transaction.is_autocommit());
}
