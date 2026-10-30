//! Durable directory observations survive API bucket deletion.
// xtask-lint: allow-file(text::uncached_statement) -- request-free: only the sync mirror's directory scan calls this module, once per bucket per scan
use super::{
    MAX_PAGE,
    root::{RootError, RootLease},
};
use crate::{
    bucket::{BucketRecord, CatalogError},
    storage::{
        admin::{AdminError, BucketDefinition, BucketName, Origin},
        catalog::{change_journal, insert_bucket_in_transaction, unavailable},
    },
};
use rusqlite::{Connection, OptionalExtension, params};

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_buckets (
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    PRIMARY KEY(root_id,bucket)
) WITHOUT ROWID;
PRAGMA user_version = 27;";

pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let tx = connection.transaction().map_err(unavailable)?;
    tx.execute_batch(SCHEMA).map_err(unavailable)?;
    tx.commit().map_err(unavailable)
}

impl RootLease {
    pub(crate) async fn ensure_sync_bucket(
        &self,
        name: &BucketName,
        project: &str,
    ) -> Result<bool, RootError> {
        let record = BucketDefinition::default_for(name, project, self.catalog().0.clock.as_ref())?
            .into_record();
        self.observe_sync_bucket(name, Some(record)).await
    }

    pub(crate) async fn remember_sync_bucket(&self, name: &BucketName) -> Result<bool, RootError> {
        self.observe_sync_bucket(name, None).await
    }

    async fn observe_sync_bucket(
        &self,
        name: &BucketName,
        mut create: Option<BucketRecord>,
    ) -> Result<bool, RootError> {
        self.file_job(|_| Ok(())).await?;
        let lease = self.clone();
        let root = self.status().root_id.as_bytes();
        let name = name.as_str().to_owned();
        self.catalog().run_admin(move |connection| {
            let _lease = lease;
            let tx = connection.transaction().map_err(unavailable)?;
            let mut incarnation: Option<String> = tx.query_row("SELECT incarnation FROM bucket_incarnations WHERE bucket=?1", [&name], |r|r.get(0)).optional().map_err(unavailable)?;
            if incarnation.is_none() {
                let known: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sync_buckets WHERE root_id=?1 AND bucket=?2)", params![root,name], |r|r.get(0)).map_err(unavailable)?;
                if known || create.is_none() { return Ok(false); }
                let record = create.as_mut().ok_or_else(|| unavailable("missing default bucket"))?;
                insert_bucket_in_transaction(&tx, record)?;
                change_journal::append(&tx, Origin::SyncIngest, change_journal::ChangeKind::BucketCreate, &name, None)?;
                incarnation = Some(tx.query_row("SELECT incarnation FROM bucket_incarnations WHERE bucket=?1", [&name], |r|r.get(0)).map_err(unavailable)?);
            }
            tx.execute("INSERT INTO sync_buckets(root_id,bucket,incarnation) VALUES(?1,?2,?3) ON CONFLICT(root_id,bucket) DO UPDATE SET incarnation=excluded.incarnation", params![root,name,incarnation]).map_err(unavailable)?;
            tx.commit().map_err(unavailable)?;
            Ok(true)
        }).await.map_err(RootError::from)
    }

    pub(crate) async fn sync_buckets_page(
        &self,
        after: String,
        limit: usize,
    ) -> Result<Vec<(String, String)>, RootError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AdminError::Invalid("invalid sync bucket page".into()).into());
        }
        let root = self.status().root_id.as_bytes();
        let lease = self.clone();
        self.catalog().run_admin(move |connection| {
            let _lease = lease;
            connection.prepare_cached("SELECT bucket,incarnation FROM sync_buckets WHERE root_id=?1 AND bucket>?2 ORDER BY bucket LIMIT ?3").map_err(unavailable)?
                .query_map(params![root,after,limit as i64], |r|Ok((r.get(0)?,r.get(1)?))).map_err(unavailable)?
                .collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
        }).await.map_err(RootError::from)
    }

    pub(crate) async fn forget_sync_bucket(
        &self,
        name: String,
        incarnation: String,
    ) -> Result<(), RootError> {
        self.file_job(|_| Ok(())).await?;
        let root = self.status().root_id.as_bytes();
        let lease = self.clone();
        self.catalog().run_admin(move |connection| {
            let _lease = lease;
            connection.execute("DELETE FROM sync_buckets WHERE root_id=?1 AND bucket=?2 AND incarnation=?3", params![root,name,incarnation]).map_err(unavailable)?;
            Ok(())
        }).await.map_err(RootError::from)
    }
}
