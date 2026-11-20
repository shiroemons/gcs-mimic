use super::*;
use crate::storage::catalog::{objects, sql};
use crate::storage::{
    admin::{BucketName, ObjectName},
    sync_state::{SyncBinding, SyncPath, findings::alias},
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
    /// The file of an object frozen as an alias changed. The edit is not taken in, since it may
    /// be meant for either object, and the stop holds a wait until the alias goes, when
    /// ingestion takes the edit in for the object left, or until the file holds its earlier
    /// bytes again or is deleted.
    AliasEdited,
}
sql::text_enum!(pub(crate) PathStop, "invalid path stop", {
    ProtectedDeletion => "ProtectedDeletion", SourceChanging => "SourceChanging", IoFailure => "IoFailure",
    EntryReplaced => "EntryReplaced", AliasEdited => "AliasEdited",
});

impl PathStop {
    pub(crate) const ALL: [Self; 5] = [
        Self::ProtectedDeletion,
        Self::SourceChanging,
        Self::IoFailure,
        Self::EntryReplaced,
        Self::AliasEdited,
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
            let stops = tx.prepare_cached("SELECT ordinal,bucket,incarnation,name FROM sync_path_stops WHERE root_id=?1 AND ordinal>?2 AND reason IN ('SourceChanging','IoFailure','EntryReplaced','AliasEdited') ORDER BY ordinal LIMIT 256")
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
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                super::super::matching(&tx, &binding, binding.bucket.as_str())
                    .map_err(unavailable)?;
                sql::exec(
                    &tx,
                    RECORD_STOP,
                    params![root, binding.bucket, name, binding.incarnation, reason],
                )?;
                tx.commit().map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }

    /// Decides what ingestion does with the changed tracked file of `name` at `path`. While
    /// another live object stands for the file as well, the edit stays out and holds a
    /// [`PathStop::AliasEdited`] stop. Once `name` went through the API and one object is left,
    /// the edit is that object's to take in. Otherwise a stop left from the freeze goes, since
    /// the edit is now taken in for `name`. Reads only when the stop already says what holds, so
    /// an unchanged freeze writes nothing.
    pub(crate) async fn observe_alias_edit(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        path: SyncPath,
    ) -> Result<AliasEdit, RootError> {
        if !self.may_hold_aliases().await? {
            return Ok(AliasEdit::Free);
        }
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            scope.authorize(&tx)?;
            super::super::matching(&tx,&binding,binding.bucket.as_str()).map_err(unavailable)?;
            let live = objects::lookup(&tx, binding.bucket.as_str(), name.as_str()).map_err(unavailable)?.is_some();
            let mut others = alias::claimants(&tx, root, &binding, path.as_str(), &name)?;
            let stopped: bool = sql::scalar(&tx, "SELECT EXISTS(SELECT 1 FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason=?5)", params![root,binding.bucket,name,binding.incarnation,PathStop::AliasEdited])?;
            let partner = others.pop();
            let edit = match (partner, live || !others.is_empty()) {
                (Some(heir), false) => return Ok(AliasEdit::HandOver(heir)),
                (Some(partner), true) => {
                    let transition = if stopped {
                        Transition::Kept
                    } else {
                        sql::exec(&tx, RECORD_STOP, params![root,binding.bucket,name,binding.incarnation,PathStop::AliasEdited])?;
                        Transition::New
                    };
                    AliasEdit::Frozen { transition, partner }
                }
                (None, _) => {
                    if stopped {
                        sql::exec(&tx, "DELETE FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason=?5", params![root,binding.bucket,name,binding.incarnation,PathStop::AliasEdited])?;
                    }
                    AliasEdit::Free
                }
            };
            tx.commit().map_err(unavailable)?;
            Ok(edit)
        }).await.map_err(RootError::from)
    }
}

/// A path stop as a wait that ended on it names it: the tracked file's path, or the object's
/// bucket and name where the tree tracks no file for it, and the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListedStop {
    pub path: String,
    pub reason: PathStop,
}

impl RootLease {
    /// The first `limit` path stops of this root, in the order they were recorded.
    pub(crate) async fn first_path_stops(&self, limit: u32) -> Result<Vec<ListedStop>, RootError> {
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                scope.authorize(&tx)?;
                tx.prepare_cached("SELECT coalesce(s.path,p.bucket||'/'||p.name),p.reason FROM sync_path_stops p LEFT JOIN sync_objects s ON s.bucket=p.bucket AND s.name=p.name AND s.incarnation=p.incarnation WHERE p.root_id=?1 ORDER BY p.ordinal LIMIT ?2")?
                    .query_map(params![root, limit], |row| {
                        Ok(ListedStop {
                            path: row.get(0)?,
                            reason: row.get(1)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }
}

/// Records one path stop of reason `?5` for the object `?3` of bucket `?2` in incarnation `?4`
/// under root `?1`, at the object's live generation, else its tracked one.
const RECORD_STOP: &str = "INSERT INTO sync_path_stops(root_id,bucket,name,owner_project,incarnation,generation,reason) SELECT ?1,?2,?3,b.project,?4,coalesce((SELECT generation FROM objects WHERE bucket=?2 AND name=?3 AND time_deleted IS NULL),(SELECT generation FROM sync_objects WHERE bucket=?2 AND name=?3 AND incarnation=?4),0),?5 FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=?2 AND i.incarnation=?4 ON CONFLICT(root_id,bucket,name,incarnation,reason) DO UPDATE SET generation=excluded.generation WHERE generation!=excluded.generation";

/// Whether a state that is reported once, when it begins, began with this observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    /// It began now, so the caller reports it.
    New,
    /// It held already and was reported when it began.
    Kept,
}

/// What ingestion does with the changed tracked file of an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AliasEdit {
    /// The file stands for `partner` as well, and the object's [`PathStop::AliasEdited`] stop
    /// holds a wait.
    Frozen {
        transition: Transition,
        partner: ObjectName,
    },
    /// The object went through the API, and the file stands for this one object alone, which
    /// takes the edit in.
    HandOver(ObjectName),
    /// The object is not frozen, and ingestion takes the file in.
    Free,
}

impl RootLease {
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
        let [first, second, third, fourth, fifth]: [Option<PathStop>; PathStop::ALL.len()] =
            std::array::from_fn(|index| reasons.get(index).copied());
        let root = self.status().root_id;
        let scope = self.control_scope();
        self.catalog().run_admin(move |connection| {
            let tx=connection.transaction()?;
            scope.authorize(&tx)?;
            sql::exec(&tx, "DELETE FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason IN (?5,?6,?7,?8,?9)", params![root,binding.bucket,name,binding.incarnation,first,second,third,fourth,fifth])?;
            tx.commit().map_err(unavailable)
        }).await.map_err(RootError::from)
    }
}
