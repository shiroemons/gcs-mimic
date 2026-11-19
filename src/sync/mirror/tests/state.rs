//! The mirror's state transitions, and the faults that drive a running mirror into a retry or a
//! pause.

use super::*;

/// A transient failure that ends the next run of a mirror once the test fires it, armed per
/// working tree like `LIVE_FAILURES`.
static RUN_FAILURES: std::sync::Mutex<Vec<(PathBuf, tokio::sync::oneshot::Receiver<()>)>> =
    std::sync::Mutex::new(Vec::new());

/// Arms a transient failure of the next run of the mirror on `tree`, which ends that run when the
/// returned sender fires. Dropping the sender disarms it.
fn fail_run_on_signal(tree: &Path) -> tokio::sync::oneshot::Sender<()> {
    let (fire, signal) = tokio::sync::oneshot::channel();
    RUN_FAILURES.lock().unwrap().push((tree.to_owned(), signal));
    fire
}

/// Runs `run`, or ends it with a transient failure when a signal armed for the mirror `config`
/// runs fires first.
pub(in crate::sync::mirror) async fn failing_on_signal(
    config: &Config,
    run: impl Future<Output = Result<(), SyncError>>,
) -> Result<(), SyncError> {
    let signal = {
        let mut armed = RUN_FAILURES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        armed
            .iter()
            .position(|(tree, _)| config.sync_dir.as_ref() == Some(tree))
            .map(|index| armed.remove(index).1)
    };
    let Some(signal) = signal else {
        return run.await;
    };
    tokio::select! {
        result = run => result,
        Ok(()) = signal => Err(SyncError::Admin(busy())),
    }
}

/// A failure of the next explicit rescan of a running mirror, armed per working tree like
/// `LIVE_FAILURES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RescanFault {
    /// The rescan fails as when catalog admission timed out.
    Busy,
    /// The rescan fails with an error that needs an operator.
    Broken,
    /// Recording the pause a failed rescan leaves behind fails once, as under contention.
    PauseRecord,
}

static RESCAN_FAULTS: std::sync::Mutex<Vec<(PathBuf, RescanFault)>> =
    std::sync::Mutex::new(Vec::new());

/// Makes the next rescan of the mirror on `tree` meet `fault`.
pub(crate) fn arm_rescan_fault(tree: &Path, fault: RescanFault) {
    RESCAN_FAULTS.lock().unwrap().push((tree.to_owned(), fault));
}

/// Whether `fault` was armed for the mirror on `tree`, which disarms it.
fn take_rescan_fault(tree: Option<&Path>, fault: RescanFault) -> bool {
    let mut armed = RESCAN_FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let index = armed
        .iter()
        .position(|(armed_tree, armed)| tree == Some(armed_tree.as_path()) && *armed == fault);
    index.map(|index| armed.remove(index)).is_some()
}

fn busy() -> AdminError {
    AdminError::Busy {
        retry_after: Duration::from_secs(1),
    }
}

pub(in crate::sync::mirror) fn injected_rescan_failure(config: &Config) -> Result<(), SyncError> {
    let tree = config.sync_dir.as_deref();
    if take_rescan_fault(tree, RescanFault::Busy) {
        return Err(SyncError::Admin(busy()));
    }
    if take_rescan_fault(tree, RescanFault::Broken) {
        return Err(SyncError::Io(io::Error::other("injected rescan failure")));
    }
    Ok(())
}

pub(in crate::sync::mirror) fn injected_pause_record_failure(
    tree: Option<&Path>,
) -> Result<(), RootError> {
    if take_rescan_fault(tree, RescanFault::PauseRecord) {
        return Err(RootError::Admin(busy()));
    }
    Ok(())
}

/// Trees whose mirror, once it begins to drop, waits for the test before it releases its root.
static HELD_RELEASES: std::sync::Mutex<Vec<(PathBuf, std::sync::mpsc::Receiver<()>)>> =
    std::sync::Mutex::new(Vec::new());

/// Trees whose held mirror has begun to drop.
static BEGUN_RELEASES: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Holds the release of the mirror's root, where a test armed it through [`hold_release`], until
/// the test lets it go, so the test can look at what `finish` does in between.
pub(crate) struct ReleaseHold(Option<(PathBuf, std::sync::mpsc::Receiver<()>)>);

impl ReleaseHold {
    pub(in crate::sync::mirror) fn armed(config: &Config) -> Self {
        let mut armed = HELD_RELEASES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let index = armed
            .iter()
            .position(|(tree, _)| config.sync_dir.as_ref() == Some(tree));
        Self(index.map(|index| armed.remove(index)))
    }
}

impl Drop for ReleaseHold {
    fn drop(&mut self) {
        if let Some((tree, release)) = self.0.take() {
            BEGUN_RELEASES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(tree);
            // A dropped sender lets the release go as well, so a failing test never hangs here.
            let _ = release.recv();
        }
    }
}

/// Holds the release of the root of the next mirror on `tree` until the returned sender fires
/// or drops.
fn hold_release(tree: &Path) -> std::sync::mpsc::Sender<()> {
    let (release, held) = std::sync::mpsc::channel();
    HELD_RELEASES.lock().unwrap().push((tree.to_owned(), held));
    release
}

/// A started mirror whose run fails transiently reports retrying until the next run has
/// reconciled the tree, and running again after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_started_mirror_retries_after_a_transient_failure_and_runs_again() {
    use MirrorState::{Retrying, Running, Starting, Stopped};
    let (data, tree, catalog, _, root) = live_fixture().await;
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    let fail = fail_run_on_signal(&tree.0);
    let task = start(catalog.clone(), root, &config).unwrap();
    mirror_reaches(&task, Running).await;
    fail.send(()).unwrap();
    let status = task.status();
    eventually_async("the mirror to run again after retrying", || async {
        status.entered().contains(&Retrying) && status.state() == Running
    })
    .await;
    // The run after the retry answers controls.
    assert!(
        tokio::time::timeout(HANG_GUARD, task.rescan())
            .await
            .unwrap()
            .unwrap()
            .baseline_complete
    );
    task.finish().await.unwrap();
    assert_eq!(
        status.entered(),
        [Starting, Running, Retrying, Running, Stopped]
    );
    catalog.close().await.unwrap();
}

/// A panic outside a supervised run, here while the mirror pauses, releases the root before
/// `finish` returns, so the directory can be owned again at once: the guard reports the mirror
/// stopped, the mirror then releases the root, and only after that does the thread drop what
/// `finish` waits for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mirror_that_panics_releases_its_root_before_finish_returns() {
    let (data, tree, catalog, _, root) = live_fixture().await;
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    LIVE_FAILURES
        .lock()
        .unwrap()
        .push((tree.0.clone(), LiveFailure::Error));
    PAUSE_PANICS.lock().unwrap().push(tree.0.clone());
    let release = hold_release(&tree.0);
    let task = start(catalog.clone(), root, &config).unwrap();
    mirror_reaches(&task, MirrorState::Stopped).await;
    eventually_async("the mirror to begin releasing its root", || async {
        BEGUN_RELEASES.lock().unwrap().contains(&tree.0)
    })
    .await;
    // The root is still held, so what `finish` waits for must still be there.
    let mut finish = std::pin::pin!(task.finish());
    assert!(
        finish
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
            .is_pending(),
        "finish returned before the mirror released its root"
    );
    release.send(()).unwrap();
    assert!(
        tokio::time::timeout(HANG_GUARD, finish)
            .await
            .unwrap()
            .is_err(),
        "the thread ended without its orderly shutdown"
    );
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

/// Fails the next explicit rescan of the running mirror on `tree` for good, with `faults` armed
/// as well, and returns the answer.
async fn fail_rescan(task: &Task, tree: &Path, faults: &[RescanFault]) -> ControlError {
    for fault in faults.iter().chain([&RescanFault::Broken]) {
        arm_rescan_fault(tree, *fault);
    }
    let failed = tokio::time::timeout(HANG_GUARD, task.rescan())
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        !RESCAN_FAULTS
            .lock()
            .unwrap()
            .iter()
            .any(|(armed, _)| armed == tree),
        "the rescan met every armed failure"
    );
    failed
}

/// A rescan of a running mirror that fails for good is answered only once the mirror is paused
/// and the catalog records the pause, so whoever reads status after the answer sees both.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_explicit_rescan_records_the_pause_before_replying() {
    use crate::storage::sync_state::observation::PauseReason;
    let (_data, tree, catalog, task) = started_mirror(None).await;
    mirror_reaches(&task, MirrorState::Running).await;
    let failed = fail_rescan(&task, &tree.0, &[]).await;
    assert!(
        matches!(
            failed,
            ControlError::RescanFailed {
                reason: PauseReason::ObserverFailure,
                change: PauseChange::Began,
                ..
            }
        ),
        "{failed:?}"
    );
    assert_eq!(task.status().state(), MirrorState::Paused);
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    assert_eq!(
        root.observation().await.unwrap().paused_reason,
        Some(PauseReason::ObserverFailure)
    );
    drop(root);
    task.finish().await.unwrap();
    catalog.close().await.unwrap();
}

/// A rescan of a running mirror that fails for good pauses synchronization even when recording
/// the pause fails: the mirror stays paused without the record, which status then shows as
/// `Paused`, and never goes through a retry that would resume synchronization without a rescan
/// that succeeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_rescan_pauses_even_when_recording_the_pause_fails() {
    use crate::storage::sync_state::observation::PauseReason;
    use MirrorState::{Paused, Running, Starting, Stopped};
    let (_data, tree, catalog, task) = started_mirror(None).await;
    mirror_reaches(&task, Running).await;
    let failed = fail_rescan(&task, &tree.0, &[RescanFault::PauseRecord]).await;
    assert!(
        matches!(
            failed,
            ControlError::RescanFailed {
                reason: PauseReason::ObserverFailure,
                change: PauseChange::Began,
                ..
            }
        ),
        "{failed:?}"
    );
    assert_eq!(task.status().state(), Paused);
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    assert_eq!(root.observation().await.unwrap().paused_reason, None);
    drop(root);
    let status = task.status();
    task.finish().await.unwrap();
    assert_eq!(status.entered(), [Starting, Running, Paused, Stopped]);
    catalog.close().await.unwrap();
}
