//! Every control read and accepted side effect checks the complete retained-owner union.

use super::{
    super::unavailable,
    root::{RootError, RootLease},
};
use crate::{auth::Identity, bucket::CatalogError};
use rusqlite::Connection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ControlAuthority {
    Permissive,
    Strict { seed: String, identity: Identity },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ControlScope {
    root: super::root::RootId,
    authority: ControlAuthority,
}
impl ControlScope {
    pub(in crate::storage) fn root_id(&self) -> super::root::RootId {
        self.root
    }
    pub(crate) fn new(root: super::root::RootId, authority: ControlAuthority) -> Self {
        Self { root, authority }
    }
    pub(in crate::storage) fn authorize(
        &self,
        connection: &Connection,
    ) -> Result<(), CatalogError> {
        self.authority.authorize(connection, self.root.as_bytes())
    }
}

impl ControlAuthority {
    pub(in crate::storage) fn require_project(&self, project: &str) -> Result<(), CatalogError> {
        if matches!(self,Self::Strict {identity,..} if !identity.has_explicit_storage_admin(project))
        {
            return Err(CatalogError::Forbidden);
        }
        Ok(())
    }
    pub(crate) fn new(seed: String, identity: Option<Identity>) -> Self {
        identity.map_or(Self::Permissive, |identity| Self::Strict { seed, identity })
    }

    pub(in crate::storage) fn authorize(
        &self,
        connection: &Connection,
        root: [u8; 16],
    ) -> Result<(), CatalogError> {
        let Self::Strict { seed, identity } = self else {
            return Ok(());
        };
        if !identity.has_explicit_storage_admin(seed) {
            return Err(CatalogError::Forbidden);
        }
        let missing:bool=connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM sync_findings f LEFT JOIN sync_finding_owners o ON o.ordinal=f.ordinal WHERE f.root_id=?1 AND (o.ordinal IS NULL OR o.owner_project IS NULL))")
            .and_then(|mut statement|statement.query_row([root],|row|row.get(0))).map_err(unavailable)?;
        if missing {
            return Err(CatalogError::Forbidden);
        }
        let mut statement=connection.prepare_cached("SELECT project FROM (SELECT project FROM buckets UNION SELECT owner AS project FROM sync_writeback_ops WHERE root_id=?1 AND phase!=4 UNION SELECT o.owner_project AS project FROM sync_finding_owners o JOIN sync_findings f ON f.ordinal=o.ordinal WHERE f.root_id=?1 UNION SELECT owner_project AS project FROM sync_path_stops WHERE root_id=?1 UNION SELECT owner AS project FROM sync_conflicts WHERE root_id=?1 AND state!=2 UNION SELECT owner AS project FROM sync_conflict_operations WHERE root_id=?1 AND (done=0 OR reclaimed=0) UNION SELECT owner_project AS project FROM sync_deletion_members m JOIN sync_deletion_sets d ON d.root_id=m.root_id WHERE m.root_id=?1 AND d.valid=1 AND m.done=0) ORDER BY project").map_err(unavailable)?;
        let mut projects = statement.query([root]).map_err(unavailable)?;
        while let Some(row) = projects.next().map_err(unavailable)? {
            let project: String = row.get(0).map_err(unavailable)?;
            if !identity.has_explicit_storage_admin(&project) {
                return Err(CatalogError::Forbidden);
            }
        }
        Ok(())
    }
}

impl RootLease {
    pub(crate) async fn authorize_controls(
        &self,
        seed: String,
        identity: Option<Identity>,
    ) -> Result<(), RootError> {
        let root = self.status().root_id.as_bytes();
        let authority = ControlAuthority::new(seed, identity);
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction().map_err(unavailable)?;
                authority.authorize(&tx, root)
            })
            .await
            .map_err(RootError::from)
    }
}
