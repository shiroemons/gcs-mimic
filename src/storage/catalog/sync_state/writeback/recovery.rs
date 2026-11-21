//! Restart recovery touches only opaque owned workspace entries, never live targets.

use super::super::SyncRecord;
use super::*;
#[cfg(test)]
use crate::storage::catalog::fault::Step;
use live_export::OwnedBody;
#[cfg(test)]
use live_export::faults::Paths;

/// The proof that recovery may remove `body`, the private entry of the armed operation `item`:
/// a regular file that no other name links and that holds exactly the operation's API copy or,
/// by the rule installation unlinks a displaced body by, the tracked body. A link elsewhere would
/// make the unlink keep bytes at that other name, and any other bytes may be an external edit.
fn owned_body(
    body: &File,
    item: &Pending,
    tracked: Option<&SyncRecord>,
) -> Result<Option<OwnedBody>, Error> {
    if !fsutil::sole_regular_link(body)? {
        return Ok(None);
    }
    if let Some(owned) = OwnedBody::api_copy(body, item)? {
        return Ok(Some(owned));
    }
    tracked.map_or(Ok(None), |tracked| OwnedBody::tracked(body, tracked))
}

/// Unlinks the private entry of `item` that `owned` proves, if there is one, then syncs the
/// workspace and the root. The unlink, the syncs and the caller's catalog completion are separate
/// durable boundaries; a restart after any of them reaches the same cancellation. `false` means
/// the entry no longer held the proved body, and nothing was unlinked or synced.
fn remove_owned(
    root: &File,
    workspace: &File,
    item: &Pending,
    owned: Option<OwnedBody>,
    #[cfg(test)] faults: &Paths,
) -> Result<bool, Error> {
    if let Some(owned) = owned {
        #[cfg(test)]
        faults.hit(Step::SyncRecoveryUnlink)?;
        if !owned.unlink(
            workspace,
            item,
            #[cfg(test)]
            faults,
        )? {
            return Ok(false);
        }
        #[cfg(test)]
        faults.hit(Step::SyncRecoveryUnlinked)?;
    }
    #[cfg(test)]
    faults.hit(Step::SyncRecoveryParentSync)?;
    workspace.sync_all()?;
    root.sync_all()?;
    #[cfg(test)]
    {
        faults.workspace_synced();
        faults.tree_synced();
        faults.hit(Step::SyncRecoveryParentDurable)?;
    }
    Ok(true)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

impl Writeback {
    pub(crate) async fn recover_pending(&self) -> Result<Status, Error> {
        self.serial_with(|this| async move { this.recover_pending_inner().await })
            .await
    }

    async fn recover_pending_inner(&self) -> Result<Status, Error> {
        let mut after = Ordinal::START;
        loop {
            let page = self.page_inner(after, PageLimit::LARGEST).await?;
            if page.is_empty() {
                break;
            }
            for item in page {
                after = item.ordinal;
                match item.phase {
                    Phase::Prepared => self.cancel_recovered(item).await?,
                    Phase::Armed => {
                        self.reclaim_owned(&item).await?;
                        self.protect_or_cancel(item).await?;
                    }
                    // Installation unlinks what it displaced before it records durability, so a
                    // durable operation leaves no body of its own to reclaim.
                    Phase::Durable => self.protect_or_cancel(item).await?,
                    Phase::Protected => {
                        if !self.lease.accepted_transfer(item.token.id).await? {
                            self.inspect_inner(item.token).await?;
                        }
                    }
                    Phase::Complete => return Err(Error::Obsolete(StaleReason::Completed)),
                }
            }
        }
        let usage = self.status_inner().await?;
        if usage.entries == 0 && !usage.over_limit() && usage.protected_entries == 0 {
            self.resume_inner().await
        } else {
            Ok(usage)
        }
    }

    /// Protects the body an armed or durable operation still has in its private entry, or
    /// cancels the operation once its absence is synced.
    async fn protect_or_cancel(&self, item: Pending) -> Result<(), Error> {
        let inspected = self.inspect_inner(item.token).await?;
        if inspected.phase == Phase::Protected {
            #[cfg(test)]
            self.recovery_protected_boundary().await?;
            Ok(())
        } else {
            self.cancel_recovered(inspected).await
        }
    }

    #[cfg(test)]
    async fn recovery_protected_boundary(&self) -> Result<(), Error> {
        let faults = self.fault_paths();
        self.fs(move |_, _| faults.hit(Step::SyncRecoveryProtected))
            .await
    }

    /// Removes what an armed operation left in its private entry when it holds no bytes that
    /// anyone else made: the operation's own API copy, which the API still holds, or the tracked
    /// body its rename displaced or removed, which installation unlinks too because the API
    /// change replaced it. A process killed after arming and before the rename, or before the
    /// unlink of what the rename displaced, leaves such a body; protecting it would report a
    /// conflict nobody made and keep `sync wait` from succeeding. Any other body stays for
    /// `inspect_inner` to protect.
    async fn reclaim_owned(&self, item: &Pending) -> Result<(), Error> {
        let (bucket, name, incarnation) =
            (item.bucket.clone(), item.name.clone(), item.incarnation);
        let tracked = self
            .db(move |connection| {
                Ok(connection
                    .prepare_cached(super::super::SELECT_BY_NAME)?
                    .query_row(
                        params![bucket, name, incarnation],
                        super::super::read_record,
                    )
                    .optional()?)
            })
            .await?;
        let selected = item.clone();
        #[cfg(test)]
        let faults = self.fault_paths();
        let result = self
            .fs(move |root, workspace| {
                let Some(body) = entry(workspace, selected.token.id)? else {
                    return Ok(());
                };
                let Some(owned) = owned_body(&body, &selected, tracked.as_ref())? else {
                    return Ok(());
                };
                // An entry that changed after its proof stays for `inspect_inner` to protect.
                remove_owned(
                    root,
                    workspace,
                    &selected,
                    Some(owned),
                    #[cfg(test)]
                    &faults,
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = result {
            self.mark_entry_failure(item.token.clone()).await?;
            return Err(error);
        }
        Ok(())
    }

    async fn cancel_recovered(&self, item: Pending) -> Result<(), Error> {
        let selected = item.clone();
        #[cfg(test)]
        let faults = self.fault_paths();
        let result = self
            .fs(move |root, workspace| {
                // Before arming, this opaque intent can contain only its private API copy; any
                // other operation's surviving body is protected.
                let owned = entry(workspace, selected.token.id)?
                    .map(|body| OwnedBody::unarmed(&body, &selected)?.ok_or(Error::Protected))
                    .transpose()?;
                if remove_owned(
                    root,
                    workspace,
                    &selected,
                    owned,
                    #[cfg(test)]
                    &faults,
                )? {
                    Ok(())
                } else {
                    Err(Error::Protected)
                }
            })
            .await;
        if let Err(error) = result {
            if error == Error::Protected {
                self.inspect_inner(item.token).await?;
                return Err(error);
            }
            self.mark_entry_failure(item.token).await?;
            return Err(error);
        }
        #[cfg(test)]
        let faults = self.fault_paths();
        self.db(move |connection| {
            let tx = connection.transaction()?;
            let current = checked(&tx, &item.token)?;
            if !matches!(
                current.phase,
                Phase::Prepared | Phase::Armed | Phase::Durable
            ) || current.retained_bytes.is_some()
            {
                return Err(Error::Protected);
            }
            #[cfg(test)]
            faults.hit(Step::SyncRecoveryComplete)?;
            live_export::finish_live(&tx, &current)?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        #[cfg(test)]
        {
            let faults = self.fault_paths();
            self.fs(move |_, _| faults.hit(Step::SyncRecoveryCompleted))
                .await?;
        }
        Ok(())
    }
}
