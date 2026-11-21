//! A tracked target whose modification time no longer reads as it was recorded while its bytes
//! stay the same, as a filesystem with coarser timestamps or a container's truncated nanoseconds
//! report it. A time that differs only says the file may differ; its hashes decide.

use super::*;
use std::time::{Duration, SystemTime};

/// The tracked target's path in the fixture's tree.
const TARGET: &str = "test-bucket/object";

/// `time` rounded down to a multiple of `precision`, or one `precision` earlier when it already
/// was one, so that it always differs.
fn coarser(time: SystemTime, precision: Duration) -> SystemTime {
    let since = time.duration_since(SystemTime::UNIX_EPOCH).unwrap();
    let precision_ns = precision.as_nanos();
    let rounded = since.as_nanos() / precision_ns * precision_ns;
    let rounded = if rounded == since.as_nanos() {
        rounded - precision_ns
    } else {
        rounded
    };
    SystemTime::UNIX_EPOCH + Duration::from_nanos(u64::try_from(rounded).unwrap())
}

/// Exports `api`, writes `bytes` over its file, which keeps the tracked size, and sets the file's
/// modification time to the recorded one at `precision`. Returns the export of `later api` that
/// replaces it, and checks that the time it reads now differs from the recorded one.
async fn begin_over_coarser(fixture: &Fixture, bytes: &[u8], precision: Duration) -> LiveExport {
    fixture.publish("object", b"api").await;
    drain(&fixture.writeback, fixture.begin("object").await).await;
    let path = fixture.tree.0.join(TARGET);
    let recorded = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, bytes).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(coarser(recorded, precision))
        .unwrap();
    assert_ne!(fs::metadata(&path).unwrap().modified().unwrap(), recorded);
    fixture.publish("object", b"later api").await;
    fixture.begin_over_tracked("object").await
}

/// The precisions a tracked file's modification time can read at: exFAT's 10 ms, and the whole
/// microseconds a container's file sharing can truncate nanoseconds to.
const PRECISIONS: [Duration; 2] = [Duration::from_millis(10), Duration::from_micros(1)];

/// A tracked target that holds the tracked bytes under a coarser modification time is no
/// external edit: a newer API generation replaces it and nothing is reported as a conflict.
#[tokio::test]
async fn a_tracked_target_whose_mtime_lost_precision_is_replaced_without_a_conflict() {
    for precision in PRECISIONS {
        let fixture = Fixture::new().await;
        let operation = begin_over_coarser(&fixture, b"api", precision).await;
        assert_eq!(
            settle(&fixture.writeback, operation).await,
            InstallOutcome::Complete,
            "{precision:?}"
        );
        assert_eq!(
            fs::read(fixture.tree.0.join(TARGET)).unwrap(),
            b"later api",
            "{precision:?}"
        );
        let usage = fixture.writeback.status().await.unwrap();
        assert_eq!(
            (usage.entries, usage.protected_entries, usage.stop()),
            (0, 0, Stop::Ready),
            "{precision:?}"
        );
        fixture.close().await;
    }
}

/// Other bytes of the tracked size under a coarser modification time are still a conflict, and
/// the target keeps them.
#[tokio::test]
async fn a_tracked_target_with_other_bytes_and_a_coarser_mtime_is_still_a_conflict() {
    for precision in PRECISIONS {
        let fixture = Fixture::new().await;
        let operation = begin_over_coarser(&fixture, b"API", precision).await;
        assert_eq!(
            settle(&fixture.writeback, operation).await,
            InstallOutcome::Conflict,
            "{precision:?}"
        );
        assert_eq!(
            fs::read(fixture.tree.0.join(TARGET)).unwrap(),
            b"API",
            "{precision:?}"
        );
        let usage = fixture.writeback.status().await.unwrap();
        assert_eq!(
            (usage.entries, usage.stop()),
            (0, Stop::Ready),
            "{precision:?}"
        );
        fixture.close().await;
    }
}
