use super::*;
use crate::storage::catalog::sql;
use crate::storage::{
    admin::{BucketName, ObjectName},
    sync_state::SyncBinding,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathStop {
    ProtectedDeletion,
    SourceChanging,
    IoFailure,
    /// A directory or special file took the place of a tracked file. Neither an export nor a
    /// deletion reaches through it, and it stops only its own path; a file inside such a
    /// directory is an entry of its own, which no object can take.
    EntryReplaced,
}
sql::text_enum!(pub(super) PathStop, "invalid path stop", {
    ProtectedDeletion => "ProtectedDeletion", SourceChanging => "SourceChanging", IoFailure => "IoFailure",
    EntryReplaced => "EntryReplaced",
});

impl PathStop {
    pub(crate) const ALL: [Self; 4] = [
        Self::ProtectedDeletion,
        Self::SourceChanging,
        Self::IoFailure,
        Self::EntryReplaced,
    ];
}

/// A path stopped by what the tree held there, a source that was changing, failed to read or
/// was replaced by another kind of entry, which a complete scan clears once the path is empty.
pub(crate) struct TransientStop {
    /// Where the stop sits in its page; the next page starts after it.
    pub ordinal: Ordinal,
    pub binding: SyncBinding,
    pub name: ObjectName,
}

impl RootLease {
    pub(crate) async fn transient_stop_page(
        &self,
        after: Ordinal,
    ) -> Result<Vec<TransientStop>, RootError> {
        let root = self.status().root_id;
        let catalog = self.status().catalog_id;
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            scope.authorize(&tx)?;
            let stops = tx.prepare_cached("SELECT ordinal,bucket,incarnation,name FROM sync_path_stops WHERE root_id=?1 AND ordinal>?2 AND reason IN ('SourceChanging','IoFailure','EntryReplaced') ORDER BY ordinal LIMIT 256")
                .and_then(|mut statement| statement.query_map(params![root,after], |row| Ok(TransientStop{ordinal:row.get(0)?,binding:SyncBinding::new(catalog,row.get::<_,BucketName>(1)?,row.get::<_,Incarnation>(2)?).with_control(scope.clone()),name:row.get(3)?}))?.collect::<rusqlite::Result<Vec<_>>>())?;
            Ok(stops)
        }).await.map_err(RootError::from)
    }

    pub(crate) async fn record_path_stop(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        reason: PathStop,
    ) -> Result<(), RootError> {
        let root = self.status().root_id;
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            super::super::matching(&tx,&binding,binding.bucket.as_str()).map_err(unavailable)?;
            sql::exec(&tx, "INSERT INTO sync_path_stops(root_id,bucket,name,owner_project,incarnation,generation,reason) SELECT ?1,?2,?3,b.project,?4,coalesce((SELECT generation FROM objects WHERE bucket=?2 AND name=?3 AND time_deleted IS NULL),(SELECT generation FROM sync_objects WHERE bucket=?2 AND name=?3 AND incarnation=?4),0),?5 FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?2 AND i.incarnation=?4 ON CONFLICT(root_id,bucket,name,incarnation,reason) DO UPDATE SET generation=excluded.generation WHERE generation!=excluded.generation", params![root,binding.bucket,name,binding.incarnation,reason])?;
            tx.commit().map_err(unavailable)
        }).await.map_err(RootError::from)
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) async fn clear_path_stop(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        reason: PathStop,
    ) -> Result<(), RootError> {
        self.clear_path_stops(binding, name, &[reason]).await
    }

    pub(crate) async fn clear_path_stops(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        reasons: &[PathStop],
    ) -> Result<(), RootError> {
        if reasons.is_empty() || reasons.len() > PathStop::ALL.len() {
            return Err(AdminError::Invalid("invalid path stop reasons".into()).into());
        }
        // One parameter per reason keeps the statement static; an unused one binds NULL, which
        // `IN` never matches. The pattern names one binding per parameter, so a reason added to
        // `PathStop::ALL` fails to compile here instead of going unmatched.
        let [first, second, third, fourth]: [Option<PathStop>; PathStop::ALL.len()] =
            std::array::from_fn(|index| reasons.get(index).copied());
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            scope.authorize(&tx)?;
            sql::exec(&tx, "DELETE FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason IN (?5,?6,?7,?8)", params![root,binding.bucket,name,binding.incarnation,first,second,third,fourth])?;
            tx.commit().map_err(unavailable)
        }).await.map_err(RootError::from)
    }
}
