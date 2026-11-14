//! Durable directory observations survive API bucket deletion.
use super::{
    MAX_PAGE, PageLimit,
    root::{RootError, RootLease},
};
use crate::storage::catalog::{After, Incarnation, bucket_incarnation, sql};
use crate::{
    bucket::BucketRecord,
    storage::{
        admin::{BucketDefinition, BucketName, Origin},
        catalog::{BucketCreation, create_bucket_in_transaction, unavailable},
    },
};
use rusqlite::{OptionalExtension, params};

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_buckets (
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    PRIMARY KEY(root_id,bucket)
) WITHOUT ROWID;
PRAGMA user_version = 27;";

/// A bucket the root has observed, with the incarnation it was observed in.
pub(crate) struct SyncBucket {
    pub(crate) name: BucketName,
    pub(crate) incarnation: Incarnation,
}

/// What `observe_sync_bucket` does when the bucket has no live incarnation.
enum MissingBucket {
    /// A tree directory appeared: create the bucket from this default definition.
    Create(Box<BucketRecord>),
    /// Only remember buckets that exist; never create one.
    Skip,
}

impl RootLease {
    pub(crate) async fn ensure_sync_bucket(
        &self,
        name: &BucketName,
        project: &str,
    ) -> Result<bool, RootError> {
        let record = BucketDefinition::default_for(name, project, self.catalog().0.clock.as_ref())?
            .into_record();
        self.observe_sync_bucket(name, MissingBucket::Create(Box::new(record)))
            .await
    }

    pub(crate) async fn remember_sync_bucket(&self, name: &BucketName) -> Result<bool, RootError> {
        self.observe_sync_bucket(name, MissingBucket::Skip).await
    }

    async fn observe_sync_bucket(
        &self,
        name: &BucketName,
        missing: MissingBucket,
    ) -> Result<bool, RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        let name = name.as_str().to_owned();
        self.catalog().run_admin(move |connection| {
            let tx = connection.transaction()?;
            lease.control_scope().authorize(&tx)?;
            let incarnation = match bucket_incarnation(&tx, &name).optional()? {
                Some(incarnation) => incarnation,
                None => {
                    let known: bool = sql::scalar(&tx, "SELECT EXISTS(SELECT 1 FROM sync_buckets WHERE root_id=?1 AND bucket=?2)", params![root,name])?;
                    if known { return Ok(false); }
                    let MissingBucket::Create(mut record) = missing else { return Ok(false) };
                    // A bucket mirroring a working tree keeps no deletion history on disk.
                    create_bucket_in_transaction(&tx, &mut record, BucketCreation::default_policy(Origin::SyncIngest, None))?;
                    bucket_incarnation(&tx, &name)?
                }
            };
            sql::exec(&tx, "INSERT INTO sync_buckets(root_id,bucket,incarnation) VALUES(?1,?2,?3) ON CONFLICT(root_id,bucket) DO UPDATE SET incarnation=excluded.incarnation", params![root,name,incarnation])?;
            tx.commit()?;
            Ok(true)
        }).await.map_err(RootError::from)
    }

    /// The buckets after `after` (from the first when `None`), in name order.
    pub(crate) async fn sync_buckets_page(
        &self,
        after: Option<BucketName>,
        limit: PageLimit<MAX_PAGE>,
    ) -> Result<Vec<SyncBucket>, RootError> {
        let after = after.map_or(After::Start, After::Key);
        let root = self.status().root_id;
        let lease = self.clone();
        self.catalog().run_admin(move |connection| {
            let tx = connection.transaction()?;
            lease.control_scope().authorize(&tx)?;
            tx.prepare_cached("SELECT bucket,incarnation FROM sync_buckets WHERE root_id=?1 AND bucket>?2 ORDER BY bucket LIMIT ?3")?
                .query_map(params![root,after,limit], |r|Ok(SyncBucket {name:r.get(0)?,incarnation:r.get(1)?}))?
                .collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
        }).await.map_err(RootError::from)
    }

    pub(crate) async fn forget_sync_bucket(
        &self,
        name: &BucketName,
        incarnation: Incarnation,
    ) -> Result<(), RootError> {
        self.revalidate().await?;
        let root = self.status().root_id;
        let lease = self.clone();
        let name = name.clone();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                lease.control_scope().authorize(&tx)?;
                sql::exec(
                    &tx,
                    "DELETE FROM sync_buckets WHERE root_id=?1 AND bucket=?2 AND incarnation=?3",
                    params![root, name, incarnation],
                )?;
                tx.commit().map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }
}
