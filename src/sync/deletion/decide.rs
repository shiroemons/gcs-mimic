//! Approving or rejecting a deletion set: the inventory that precedes a decision, the decision
//! itself, and what follows it, which runs the approved set or writes the rejected one back.

use super::{
    Begun, Config, Guard, OpaqueId, Recheck, RootLease, Taken, execute_with, take_inventory,
    write_back_rejected,
};
use crate::{
    diagnostics::LogLevel,
    storage::{admin::AdminError, sync_state::deletions::DeletionSet, sync_state::root::RootError},
    sync::{
        SyncError,
        control::{ControlError, Decision},
        interrupt::Interrupt,
        mirror::LiveScope,
    },
};

/// What the inventory before a decision found about the set the decision names.
#[derive(Debug)]
enum Rechecked {
    /// The tree shows the set's members as they were, so the revision stands.
    Confirmed,
    /// The tree shows other members, and a new set under a new revision replaced it.
    Replaced,
    /// The set did not wait for a decision; the set stays as stored.
    NotAwaiting,
    /// An API or bucket change moved the journal while the inventory compared the members, as
    /// the error says, so the set was not compared with the tree at the current fence. The
    /// decision is refused without being recorded.
    Superseded(SyncError),
    /// A stop ended the inventory before it compared every member, so nothing is known about
    /// the set.
    Interrupted,
}

/// Inventories the tree again before an approval or rejection of the set `revision`, when that
/// set still waits for one, so that the decision applies to what the tree shows now: a native
/// notification no longer withdraws such a set. Finding the same members keeps the revision,
/// whatever notification arrived since the last pass; finding others replaces the set, and the
/// decision then fails as stale, naming the new one. Only the members decide, as in every
/// inventory, so an API write since the set was taken keeps the revision unless it touched a
/// member; an update of a member's bucket, which may make the deletions unrecoverable, replaces
/// the set whatever the tree shows. A valid waiting set means the last pass let
/// deletions be inferred, so no gate is consulted. A change of the journal during this
/// inventory supersedes it, and the decision is refused without trying to record it.
async fn recheck(
    root: &RootLease,
    config: &Config,
    revision: OpaqueId,
    interrupt: Interrupt<'_>,
) -> Result<Rechecked, SyncError> {
    let guard = Guard::from(config.sync_delete_guard);
    let inventory = match root.begin_deletion_recheck(guard, revision).await? {
        Recheck::Confirm(inventory) => inventory,
        Recheck::Replace => match root.begin_new_deletion_inventory(guard).await? {
            Begun::Started(inventory) => inventory,
            Begun::RejectionPending => return Ok(Rechecked::NotAwaiting),
        },
        Recheck::NotAwaiting => return Ok(Rechecked::NotAwaiting),
    };
    match take_inventory(root, guard, inventory, interrupt).await {
        Ok(Taken::Confirmed(_)) => Ok(Rechecked::Confirmed),
        Ok(Taken::Finished(_)) => Ok(Rechecked::Replaced),
        // A rejection left between the two inventories is not a decision's to wait for, and
        // ends it as a stop does.
        Ok(Taken::Interrupted | Taken::RejectionPending) => Ok(Rechecked::Interrupted),
        Err(error) if error.is_stale_deletion() => Ok(Rechecked::Superseded(error)),
        Err(error) => Err(error),
    }
}

/// Records `decision` on the deletion set `revision` for `identity`, after the inventory that
/// makes sure the set is the one the tree shows. The caller answers with the result before it
/// follows a recorded decision.
pub(in crate::sync) async fn record(
    scope: LiveScope<'_>,
    decision: Decision,
    revision: OpaqueId,
    identity: Option<crate::auth::Identity>,
) -> Result<DeletionSet, ControlError> {
    decidable(scope, decision, revision, identity).await?;
    apply(scope, decision, revision).await
}

/// Follows a recorded `decision` on `set`: runs the approved set, or writes the rejected set's
/// files back from their objects.
pub(in crate::sync) async fn follow(
    scope: LiveScope<'_>,
    decision: Decision,
    set: DeletionSet,
) -> Result<(), SyncError> {
    let interrupt = Interrupt::new(scope.stopping);
    match decision {
        Decision::Approve => {
            execute_with(scope.catalog, scope.root, set, interrupt)
                .await?
                .report(LogLevel::Info);
        }
        Decision::Reject => {
            write_back_rejected(scope.catalog, scope.root, interrupt).await?;
        }
    }
    Ok(())
}

/// Authorizes `decision` on the deletion set `revision` for `identity`, then inventories the
/// tree again if the set still waits for the decision, so that a native notification since
/// the last pass that left its members as they were does not make it stale. A stop or an
/// API change during that inventory refuses the decision: the set was not compared with the
/// tree at the current fence, so recording a decision on it would apply to members nobody
/// confirmed. The refusal does not rely on the decision's own check of the fence.
async fn decidable(
    scope: LiveScope<'_>,
    decision: Decision,
    revision: OpaqueId,
    identity: Option<crate::auth::Identity>,
) -> Result<(), ControlError> {
    scope
        .root
        .authorize_controls(scope.config.seed_project.clone(), identity)
        .await?;
    match recheck(
        scope.root,
        scope.config,
        revision,
        Interrupt::new(scope.stopping),
    )
    .await
    .map_err(ControlError::Failed)?
    {
        Rechecked::Interrupted => Err(ControlError::Shutdown),
        Rechecked::Superseded(error) => Err(refuse(scope, decision, revision, error).await),
        // The decision itself refuses a set that is no longer the one waiting.
        Rechecked::Confirmed | Rechecked::Replaced | Rechecked::NotAwaiting => Ok(()),
    }
}

/// Refuses `decision` on the set `revision`, which `error` found no longer decidable, with
/// what became of the set, read right after on this worker, which alone takes inventories,
/// so that the answer can say what to do instead of trying again.
async fn refuse(
    scope: LiveScope<'_>,
    decision: Decision,
    revision: OpaqueId,
    error: SyncError,
) -> ControlError {
    match scope.root.refusal(revision).await {
        Ok(refusal) => ControlError::Stale {
            decision,
            revision,
            refusal,
            error,
        },
        Err(failed) => failed.into(),
    }
}

/// Records `decision` on the set `revision`. A set that no longer waits for it is refused
/// with what became of it.
async fn apply(
    scope: LiveScope<'_>,
    decision: Decision,
    revision: OpaqueId,
) -> Result<DeletionSet, ControlError> {
    let root = scope.root;
    let applied = match decision {
        Decision::Approve => root.approve_deletions(revision).await,
        Decision::Reject => root.reject_deletions(revision).await,
    };
    let error = match applied {
        Ok(set) => return Ok(set),
        Err(error) => error,
    };
    if !matches!(error, RootError::Admin(AdminError::Precondition)) {
        return Err(error.into());
    }
    Err(refuse(scope, decision, revision, error.into()).await)
}
