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
