//! Readers retain completed deletion membership while live safety findings continue to refresh.

use super::*;
use crate::storage::{change_journal::JournalFence, sync_state::deletions::DeletionSet};
use std::sync::{Arc, Mutex};

pub(in crate::storage::catalog::sync_state) type Published = Arc<Mutex<Option<Arc<Snapshot>>>>;

#[derive(Clone)]
pub(in crate::storage::catalog::sync_state) struct Snapshot {
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
            *published
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
    }
}

fn snapshot(
    connection: &Connection,
    root: super::super::root::RootId,
) -> Result<Snapshot, CatalogError> {
    let fence = super::super::enabled_fence(connection).optional()?;
    Ok(Snapshot {
        deletions: super::super::deletions::read_set(connection, root)?,
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
        let (summary, set) = self
            .catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                // Cached observations never cache authorization or survive a catalog mutation.
                authority.authorize(&tx, root)?;
                let current = snapshot(&tx, root)?;
                let selected = published
                    .as_deref()
                    .filter(|published| published.fence == current.fence)
                    .unwrap_or(&current);
                Ok((read_summary(&tx, root)?, selected.deletions.clone()))
            })
            .await?;
        Ok((summary, set.map(|set| self.with_cause(set))))
    }
}
