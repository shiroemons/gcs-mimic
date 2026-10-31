//! Restart recovery touches only opaque owned workspace entries, never live targets.

use super::*;
#[cfg(all(test, unix))]
use crate::storage::catalog::fault;
#[cfg(test)]
use crate::storage::catalog::fault::Step;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

impl Writeback {
    pub(crate) async fn recover_pending(&self) -> Result<Status, Error> {
        let this = self.clone();
        self.serial(async move { this.recover_pending_inner().await })
            .await
    }

    async fn recover_pending_inner(&self) -> Result<Status, Error> {
        let mut after = 0;
        loop {
            let page = self.page_inner(after, 256).await?;
            if page.is_empty() {
                break;
            }
            for item in page {
                after = item.ordinal;
                match item.phase {
                    Phase::Prepared => self.cancel_recovered(item).await?,
                    Phase::Armed | Phase::Durable => {
                        let inspected = self.inspect_inner(item.token).await?;
                        if inspected.phase != Phase::Protected {
                            self.cancel_recovered(inspected).await?;
                        } else {
                            #[cfg(test)]
                            self.recovery_protected_boundary().await?;
                        }
                    }
                    Phase::Protected => {
                        if !self
                            .lease
                            .accepted_transfer(&item.token.id)
                            .await
                            .map_err(AdminError::backend)?
                        {
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

    #[cfg(test)]
    async fn recovery_protected_boundary(&self) -> Result<(), Error> {
        let faults = self.fault_paths();
        self.fs(move |_, _| faults.hit(Step::SyncRecoveryProtected))
            .await
    }

    async fn cancel_recovered(&self, item: Pending) -> Result<(), Error> {
        let selected = item.clone();
        #[cfg(test)]
        let faults = self.fault_paths();
        let result = self
            .fs(move |root, workspace| {
                let body = entry(workspace, &selected.token.id)?;
                if body.is_some()
                    && (selected.phase != Phase::Prepared || selected.kind != Kind::Publish)
                {
                    return Err(Error::Protected);
                }
                // Before arming, this opaque intent can contain only its private API copy.
                if let Some(body) = body {
                    #[cfg(test)]
                    faults.hit(Step::SyncRecoveryUnlink)?;
                    #[cfg(all(test, unix))]
                    fault::unlinked(
                        &faults
                            .tree
                            .join(mapping::WORKSPACE)
                            .join(&selected.token.id),
                        &body,
                    )?;
                    fsutil::unlink_file_at(workspace, selected.token.id.as_ref())?;
                    #[cfg(test)]
                    faults.hit(Step::SyncRecoveryUnlinked)?;
                    #[cfg(not(all(test, unix)))]
                    drop(body);
                }
                #[cfg(test)]
                faults.hit(Step::SyncRecoveryParentSync)?;
                workspace.sync_all()?;
                root.sync_all()?;
                #[cfg(test)]
                {
                    #[cfg(unix)]
                    fault::dir_synced(&faults.tree.join(mapping::WORKSPACE));
                    #[cfg(unix)]
                    fault::dir_synced(&faults.tree);
                    faults.hit(Step::SyncRecoveryParentDurable)?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = result {
            if error == Error::Protected {
                self.inspect_inner(item.token).await?;
                return Err(error);
            }
            self.io_failure(item.token, 1).await?;
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
