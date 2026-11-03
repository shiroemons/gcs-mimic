//! Readers retain the last complete observation while the actor rebuilds deletion membership.

use super::*;
use crate::storage::{change_journal::JournalFence, sync_state::deletions::DeletionSet};
use std::sync::{Arc, Mutex};

pub(in crate::storage::catalog::sync_state) type Published = Arc<Mutex<Option<Arc<Snapshot>>>>;

#[derive(Clone)]
pub(in crate::storage::catalog::sync_state) struct Snapshot {
    summary: Summary,
    deletions: Option<DeletionSet>,
    fence: Option<JournalFence>,
}

pub(crate) struct Publication {
    published: Option<Published>,
}

fn published_lock(
    published: &Published,
) -> Result<std::sync::MutexGuard<'_, Option<Arc<Snapshot>>>, RootError> {
    published
        .lock()
        .map_err(|_| AdminError::backend(std::sync::PoisonError::new(())).into())
}
impl Drop for Publication {
    fn drop(&mut self) {
        if let Some(published) = &self.published {
            // A poisoned lock remains visibly poisoned to subsequent readers.
            *published.lock().unwrap_or_else(|error| error.into_inner()) = None;
        }
    }
}

fn snapshot(
    connection: &Connection,
    root: super::super::root::RootId,
) -> Result<Snapshot, CatalogError> {
    let fence = connection
        .prepare_cached(
            "SELECT sequence,epoch FROM change_journal_state WHERE singleton=1 AND enabled=1",
        )?
        .query_row([], |row| {
            Ok(JournalFence {
                sequence: row.get(0)?,
                revision: row.get(1)?,
            })
        })
        .optional()?;
    Ok(Snapshot {
        summary: read_summary(connection, root)?,
        deletions: super::super::deletions::read_set(connection, root.as_bytes())?,
        fence,
    })
}

impl RootLease {
    pub(crate) async fn begin_observation_publication(&self) -> Result<Publication, RootError> {
        if published_lock(&self.published_observation)?.is_some() {
            return Ok(Publication { published: None });
        }
        let root = self.status().root_id;
        let scope = self.control_scope();
        let current = self
            .catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                scope.authorize(&tx)?;
                snapshot(&tx, root)
            })
            .await?;
        *published_lock(&self.published_observation)? = Some(Arc::new(current));
        Ok(Publication {
            published: Some(self.published_observation.clone()),
        })
    }

    pub(crate) async fn published_observation_for(
        &self,
        authority: super::super::authority::ControlAuthority,
    ) -> Result<(Summary, Option<DeletionSet>), RootError> {
        let published = published_lock(&self.published_observation)?.clone();
        let root = self.status().root_id;
        Ok(self
            .catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                // Cached observations never cache authorization or survive a catalog mutation.
                authority.authorize(&tx, root.as_bytes())?;
                let current = snapshot(&tx, root)?;
                let selected = published
                    .as_deref()
                    .filter(|published| published.fence == current.fence)
                    .unwrap_or(&current);
                Ok((selected.summary.clone(), selected.deletions.clone()))
            })
            .await?)
    }
}
