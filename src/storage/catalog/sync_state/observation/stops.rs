use super::*;
use crate::storage::catalog::sql;
use crate::storage::{admin::ObjectName, sync_state::SyncBinding};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PathStop {
    ProtectedDeletion,
    SourceChanging,
    IoFailure,
}
sql::text_enum!(pub(super) PathStop, "invalid path stop", {
    ProtectedDeletion => "ProtectedDeletion", SourceChanging => "SourceChanging", IoFailure => "IoFailure",
});

impl RootLease {
    pub(crate) async fn transient_stop_page(
        &self,
        after: i64,
    ) -> Result<Vec<(i64, SyncBinding, ObjectName)>, RootError> {
        let root = self.status().root_id.as_bytes();
        let catalog = self.status().catalog_id;
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            scope.authorize(&tx)?;
            let values = tx.prepare_cached("SELECT ordinal,bucket,incarnation,name FROM sync_path_stops WHERE root_id=?1 AND ordinal>?2 AND reason IN ('SourceChanging','IoFailure') ORDER BY ordinal LIMIT 256")
                .and_then(|mut statement| statement.query_map(params![root,after], |row| Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>())?;
            values.into_iter().map(|(ordinal,bucket,incarnation,name)| Ok((ordinal,SyncBinding {catalog,bucket,incarnation,deletion:None,control:Some(scope.clone())},ObjectName::new(name).map_err(unavailable)?))).collect()
        }).await.map_err(RootError::from)
    }

    pub(crate) async fn record_path_stop(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        reason: PathStop,
    ) -> Result<(), RootError> {
        let root = self.status().root_id.as_bytes();
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            super::super::matching(&tx,&binding,&binding.bucket).map_err(unavailable)?;
            sql::exec(&tx, "INSERT INTO sync_path_stops(root_id,bucket,name,owner_project,incarnation,generation,reason) SELECT ?1,?2,?3,b.project,?4,coalesce((SELECT generation FROM objects WHERE bucket=?2 AND name=?3 AND time_deleted IS NULL),(SELECT generation FROM sync_objects WHERE bucket=?2 AND name=?3 AND incarnation=?4),0),?5 FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?2 AND i.incarnation=?4 ON CONFLICT(root_id,bucket,name,incarnation,reason) DO UPDATE SET generation=excluded.generation WHERE generation!=excluded.generation", params![root,binding.bucket,name.as_str(),binding.incarnation,reason])?;
            tx.commit().map_err(unavailable)
        }).await.map_err(RootError::from)
    }

    #[cfg(test)]
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
        if reasons.is_empty() || reasons.len() > 3 {
            return Err(AdminError::Invalid("invalid path stop reasons".into()).into());
        }
        let reasons: [Option<PathStop>; 3] =
            std::array::from_fn(|index| reasons.get(index).copied());
        let root = self.status().root_id.as_bytes();
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            scope.authorize(&tx)?;
            sql::exec(&tx, "DELETE FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason IN (?5,?6,?7)", params![root,binding.bucket,name.as_str(),binding.incarnation,reasons[0],reasons[1],reasons[2]])?;
            tx.commit().map_err(unavailable)
        }).await.map_err(RootError::from)
    }
}
