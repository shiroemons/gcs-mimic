//! Restart recovery touches only opaque owned workspace entries, never live targets.

use super::super::SyncRecord;
use super::*;
#[cfg(test)]
use crate::storage::catalog::fault::Step;
#[cfg(test)]
use live_export::faults::Paths;

/// The identity of `body`, the private entry of the armed operation `item`, when recovery may
/// remove it: a regular file that no other name links and that, unchanged while it was hashed,
/// holds exactly the operation's API copy or the tracked body. A link elsewhere would make the
/// unlink keep bytes at that other name, and any other bytes may be an external edit.
fn owned_body(
    body: &File,
    item: &Pending,
    tracked: Option<&SyncRecord>,
) -> Result<Option<fsutil::FileIdentity>, Error> {
    if !fsutil::sole_regular_link(body)? {
        return Ok(None);
    }
    if let Some(identity) = live_export::verified_identity(body, item.size, item.md5, item.crc32c)?
    {
        return Ok(Some(identity));
    }
    let Some(tracked) = tracked else {
        return Ok(None);
    };
    let fingerprint = &tracked.fingerprint;
    live_export::verified_identity(body, fingerprint.size, fingerprint.md5, fingerprint.crc32c)
}

/// Unlinks `body`, the private entry of `item`, if there is one, then syncs the workspace and
/// the root. The unlink, the syncs and the caller's catalog completion are separate durable
/// boundaries; a restart after any of them reaches the same cancellation.
fn remove_owned(
    root: &File,
    workspace: &File,
    item: &Pending,
    body: Option<File>,
    #[cfg(test)] faults: &Paths,
) -> Result<(), Error> {
    if let Some(body) = body {
        #[cfg(test)]
        faults.hit(Step::SyncRecoveryUnlink)?;
        #[cfg(test)]
        faults.unlinking(item, &body)?;
        fsutil::unlink_file_at(workspace, item.token.id.to_hex().as_ref())?;
        #[cfg(test)]
        faults.hit(Step::SyncRecoveryUnlinked)?;
        #[cfg(not(test))]
        drop(body);
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
    Ok(())
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
                    Phase::Complete => return Err(stale()),
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
                let Some(identity) = owned_body(&body, &selected, tracked.as_ref())? else {
                    return Ok(());
                };
                // The name must still hold the body just hashed when it is unlinked.
                let current = entry(workspace, selected.token.id)?
                    .map(|current| fsutil::stat_identity(&current))
                    .transpose()?;
                if current != Some(identity) {
                    return Ok(());
                }
                remove_owned(
                    root,
                    workspace,
                    &selected,
                    Some(body),
                    #[cfg(test)]
                    &faults,
                )
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
                let body = entry(workspace, selected.token.id)?;
                if body.is_some()
                    && (selected.phase != Phase::Prepared || selected.kind != Kind::Publish)
                {
                    return Err(Error::Protected);
                }
                // Before arming, this opaque intent can contain only its private API copy.
                remove_owned(
                    root,
                    workspace,
                    &selected,
                    body,
                    #[cfg(test)]
                    &faults,
                )
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
