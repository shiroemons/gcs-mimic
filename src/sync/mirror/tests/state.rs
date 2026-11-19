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

/// Whether `fault` was armed for the mirror `config` runs, which disarms it.
fn take_rescan_fault(config: &Config, fault: RescanFault) -> bool {
    let mut armed = RESCAN_FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let index = armed
        .iter()
        .position(|(tree, armed)| config.sync_dir.as_ref() == Some(tree) && *armed == fault);
    index.map(|index| armed.remove(index)).is_some()
}

fn busy() -> AdminError {
    AdminError::Busy {
        retry_after: Duration::from_secs(1),
    }
}

pub(in crate::sync::mirror) fn injected_rescan_failure(config: &Config) -> Result<(), SyncError> {
    if take_rescan_fault(config, RescanFault::Busy) {
        return Err(SyncError::Admin(busy()));
    }
    if take_rescan_fault(config, RescanFault::Broken) {
        return Err(SyncError::Io(io::Error::other("injected rescan failure")));
    }
    Ok(())
}

pub(in crate::sync::mirror) fn injected_pause_record_failure(
    config: &Config,
) -> Result<(), RootError> {
    if take_rescan_fault(config, RescanFault::PauseRecord) {
        return Err(RootError::Admin(busy()));
    }
    Ok(())
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

/// A rescan of a running mirror that fails for good pauses synchronization even when recording
/// the pause fails for a moment: the mirror goes straight to the pause and records it there,
/// never through a retry that would resume synchronization without a rescan that succeeded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_rescan_pauses_even_when_recording_the_pause_fails_once() {
    use crate::storage::sync_state::observation::PauseReason;
    use MirrorState::{Paused, Running, Starting, Stopped};
    let (_data, tree, catalog, task) = started_mirror(None).await;
    mirror_reaches(&task, Running).await;
    arm_rescan_fault(&tree.0, RescanFault::Broken);
    arm_rescan_fault(&tree.0, RescanFault::PauseRecord);
    let failed = tokio::time::timeout(HANG_GUARD, task.rescan())
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(
            failed,
            ControlError::RescanFailed {
                reason: PauseReason::ObserverFailure,
                ..
            }
        ),
        "{failed:?}"
    );
    assert!(
        !RESCAN_FAULTS
            .lock()
            .unwrap()
            .iter()
            .any(|(armed, _)| *armed == tree.0),
        "the rescan met the armed failure"
    );
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    await_paused(&root).await;
    assert_eq!(
        root.observation().await.unwrap().paused_reason,
        Some(PauseReason::ObserverFailure)
    );
    assert_eq!(task.status().state(), Paused);
    drop(root);
    let status = task.status();
    task.finish().await.unwrap();
    assert_eq!(status.entered(), [Starting, Running, Paused, Stopped]);
    catalog.close().await.unwrap();
}
