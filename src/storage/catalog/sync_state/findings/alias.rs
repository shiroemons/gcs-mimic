//! The spelling under which the tree holds the one file of objects whose names the root's file
//! system takes for the same name, beside their `alias` findings. The file stands for every
//! object of the alias while more than one of them lives, so none of them is exported over it,
//! ingested from it or deleted because it went. Only an API change ends an alias, and its
//! findings go with the export or ingestion that settles the file, in the same transaction: an
//! object whose finding went first would count as tracked and present, and a file it no longer
//! held would read as its deletion.
use super::*;
use crate::storage::{
    admin::BucketName,
    sync_state::{
        SyncFingerprint, SyncPath,
        observation::{PathStop, Transition},
    },
};
use std::sync::atomic::{AtomicU8, Ordering};

/// The schema that keeps an alias finding's spelling. The number is written only here:
/// [`migrate`] stores it, so that reordering it against another lane's schema changes one line.
pub(in crate::storage) const SCHEMA_VERSION: i64 = 62;
/// A spelling goes with its finding, and with the `alias` reason once the finding gets another
/// one; a spelling that changes moves the findings revision as the finding itself does.
/// `released` marks an alias that ended and waits for the export that settles its file, which
/// is reported once. `edit_base` is the object's live generation when an edit of the file was
/// first seen, the one generation that edit may replace once the object is the last of the
/// alias; a generation written after it would otherwise be lost to the edit.
pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_finding_aliases (
 ordinal INTEGER PRIMARY KEY REFERENCES sync_findings(ordinal) ON DELETE CASCADE,
 spelling TEXT NOT NULL COLLATE BINARY CHECK(length(spelling) BETWEEN 1 AND 4096),
 released INTEGER NOT NULL DEFAULT 0 CHECK(released IN (0,1)),
 edit_base INTEGER CHECK(edit_base IS NULL OR edit_base>=0)
);
CREATE INDEX sync_finding_aliases_spelling ON sync_finding_aliases(spelling);
CREATE TRIGGER sync_finding_alias_reason AFTER UPDATE OF reason ON sync_findings WHEN NEW.reason!='alias' BEGIN
 DELETE FROM sync_finding_aliases WHERE ordinal=NEW.ordinal;
END;
CREATE TRIGGER sync_observation_finding_alias_update AFTER UPDATE OF spelling ON sync_finding_aliases BEGIN
 UPDATE sync_observations SET revision=revision+1,mapping_findings_revision=mapping_findings_revision+1 WHERE root_id=(SELECT root_id FROM sync_findings WHERE ordinal=NEW.ordinal);
END;";

/// Upgrades to [`SCHEMA_VERSION`] in one transaction. No earlier schema recorded an `alias`
/// finding, so there is nothing to carry over.
pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit().map_err(unavailable)
}

/// The live objects other than `?5` that the file at spelling `?4` stands for, in bucket `?2` of
/// incarnation `?3` under root `?1`: those whose alias findings name the spelling, and the one
/// the tree tracks there. The one rule for whether a file is frozen: it is while it stands for
/// another live object as well. Two names tell an alias that holds apart from one that ends.
const CLAIMANTS: &str = "SELECT f.name FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal JOIN objects o ON o.bucket=f.bucket AND o.name=f.name AND o.time_deleted IS NULL WHERE f.root_id=?1 AND f.bucket=?2 AND f.incarnation=?3 AND a.spelling=?4 AND f.name!=?5
 UNION SELECT s.name FROM sync_objects s JOIN objects o ON o.bucket=s.bucket AND o.name=s.name AND o.time_deleted IS NULL WHERE s.bucket=?2 AND s.incarnation=?3 AND s.path=?4 AND s.name!=?5 LIMIT 2";

/// The live objects other than `name` that the file at `spelling` stands for, at most two.
pub(in crate::storage) fn claimants(
    transaction: &Transaction<'_>,
    root: RootId,
    binding: &SyncBinding,
    spelling: &str,
    name: &ObjectName,
) -> rusqlite::Result<Vec<ObjectName>> {
    transaction
        .prepare_cached(CLAIMANTS)?
        .query_map(
            params![root, binding.bucket, binding.incarnation, spelling, name],
            |row| row.get(0),
        )?
        .collect()
}

/// Records, as an edit of the file at spelling `?4` that `?5` tracks is first seen, the live
/// generation of every other object whose alias finding names that spelling, in bucket `?2` of
/// incarnation `?3` under root `?1`. A later edit of the same file keeps the first base: the
/// file never showed what the API wrote to those objects since.
const NOTE_EDIT_BASE: &str = "UPDATE sync_finding_aliases SET edit_base=(SELECT o.generation FROM sync_findings f JOIN objects o ON o.bucket=f.bucket AND o.name=f.name AND o.time_deleted IS NULL WHERE f.ordinal=sync_finding_aliases.ordinal) WHERE spelling=?4 AND ordinal IN (SELECT ordinal FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND incarnation=?3 AND name!=?5)";

/// Notes the generation each other object of the alias at `spelling` has as the edit of its
/// file, tracked for `holder`, is first seen; see [`NOTE_EDIT_BASE`].
pub(in crate::storage) fn note_edit_base(
    transaction: &Transaction<'_>,
    root: RootId,
    binding: &SyncBinding,
    spelling: &str,
    holder: &ObjectName,
) -> rusqlite::Result<()> {
    transaction
        .prepare_cached(NOTE_EDIT_BASE)?
        .execute(params![
            root,
            binding.bucket,
            binding.incarnation,
            spelling,
            holder
        ])?;
    Ok(())
}

/// The generation of `heir` that an edit of the file it shared may replace: the one noted when
/// the edit was first seen, or, for an edit first seen once `heir` was the last of the alias,
/// the generation its alias finding last followed. `None` when `heir` holds no alias finding.
pub(in crate::storage) fn edit_base(
    transaction: &Transaction<'_>,
    root: RootId,
    binding: &SyncBinding,
    heir: &ObjectName,
) -> rusqlite::Result<Option<Generation>> {
    transaction
        .prepare_cached("SELECT coalesce(a.edit_base,f.generation) FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3 AND f.incarnation=?4")?
        .query_row(params![root, binding.bucket, heir, binding.incarnation], |row| row.get(0))
        .optional()
}

/// Each spelling of root `?1` with the number of live objects its file stands for, those that
/// stand for more than one: the objects whose alias findings name it, and the one tracked there.
const FROZEN_FILES: &str = "SELECT spelling,count(*) FROM (
 SELECT a.spelling AS spelling,f.name AS name FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal JOIN objects o ON o.bucket=f.bucket AND o.name=f.name AND o.time_deleted IS NULL WHERE f.root_id=?1
 UNION SELECT s.path,s.name FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal JOIN sync_objects s ON s.bucket=f.bucket AND s.incarnation=f.incarnation AND s.path=a.spelling JOIN objects o ON o.bucket=s.bucket AND o.name=s.name AND o.time_deleted IS NULL WHERE f.root_id=?1
) GROUP BY spelling HAVING count(*)>1 ORDER BY spelling";

/// A file of the tree that stands for several live objects, which stays frozen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FrozenFile {
    /// The file's path below the root.
    pub path: String,
    /// How many live objects it stands for.
    pub objects: u64,
}

/// Whether a root may hold alias findings, which every lookup of one asks first: most roots
/// never hold one, and their exports and ingestions then read nothing more for aliases.
#[derive(Debug, Default)]
pub(crate) struct AliasPresence(AtomicU8);

impl AliasPresence {
    const UNKNOWN: u8 = 0;
    const NONE: u8 = 1;
    const SOME: u8 = 2;

    /// Notes an alias finding about to be recorded. It stays noted until the process ends,
    /// which costs only the reads a root without aliases skips.
    fn note(&self) {
        self.0.store(Self::SOME, Ordering::Release);
    }
}

/// What recording an alias did to one object's finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marked {
    /// The finding became an alias finding now, which the caller reports.
    New,
    /// It was an alias finding already.
    Kept,
    /// It keeps another reason: an export conflict holds an edit, and another cause of the
    /// object's own, such as a collision, is its own observation's to clear.
    Other,
}

/// What recording an alias did to the objects it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AliasRecorded {
    pub own: Marked,
    /// The other object the tree tracks at the spelling, when the API still holds it.
    pub partner: Option<(ObjectName, Marked)>,
}

/// Whether an object stays frozen, decided before its file is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AliasHold {
    /// No alias names the object or its file.
    NotAliased,
    /// Another live object of the alias stands for the same file, so it stays as it is.
    Held,
    /// The alias ended: the finding naming `spelling` stays until the export that settles the
    /// file records it, and `transition` tells whether it ended with this observation.
    Released {
        spelling: String,
        transition: Transition,
    },
}

/// What the deletion of an object the tree tracks does to its file, which other objects of an
/// alias may stand for too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Holding {
    /// No other live object stands for the file, or the one left does and the file holds no
    /// edit: the file goes with the deleted object, and the object left gets its own.
    Alone,
    /// Other live objects stand for the file, which stays frozen for them under the deleted
    /// object's record.
    Shared,
    /// One live object is left and the file holds an edit made while frozen, which ingestion
    /// takes in for that object before the file goes.
    Edited,
}

impl RootLease {
    /// Whether this root may hold alias findings; see [`AliasPresence`].
    pub(crate) async fn may_hold_aliases(&self) -> Result<bool, RootError> {
        let presence = self.alias_presence();
        match presence.0.load(Ordering::Acquire) {
            AliasPresence::NONE => return Ok(false),
            AliasPresence::SOME => return Ok(true),
            _ => {}
        }
        let root = self.status().root_id;
        let found: bool = self
            .catalog()
            .run_admin(move |c| {
                sql::scalar(c, "SELECT EXISTS(SELECT 1 FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1)", [root])
                    .map_err(unavailable)
            })
            .await?;
        // A concurrent note wins over a read that found none.
        let _ = presence.0.compare_exchange(
            AliasPresence::UNKNOWN,
            if found {
                AliasPresence::SOME
            } else {
                AliasPresence::NONE
            },
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        Ok(presence.0.load(Ordering::Acquire) == AliasPresence::SOME)
    }

    /// Records `name`, live at `generation`, as an alias whose file the tree holds at
    /// `spelling`, and the object tracked at `spelling` too while the API holds it, at its live
    /// generation. Each finding follows [`RootLease::record_finding`]; the tracked object keeps
    /// a finding of another reason. Recording what the rows already say only reads.
    pub(crate) async fn record_alias(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
        spelling: String,
    ) -> Result<AliasRecorded, RootError> {
        self.revalidate().await?;
        self.alias_presence().note();
        let lease = self.clone();
        let root = self.status().root_id;
        let recorded = self
            .catalog()
            .run_admin_with_error(move |c| {
                let tx = c.transaction().map_err(unavailable)?;
                lease.control_scope().authorize(&tx)?;
                matching(&tx, &binding, binding.bucket.as_str())?;
                let own = write_alias(&tx, root, &binding, &name, generation, &spelling)?;
                let partner: Option<(ObjectName, Generation)> = sql::row(&tx, "SELECT s.name,o.generation FROM sync_objects s JOIN objects o ON o.bucket=s.bucket AND o.name=s.name AND o.time_deleted IS NULL WHERE s.bucket=?1 AND s.incarnation=?2 AND s.path=?3 AND s.name!=?4", params![binding.bucket, binding.incarnation, spelling, name], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(unavailable)?;
                let partner = match partner {
                    Some((partner, live)) => {
                        let stored: Option<FindingKind> = sql::optional_scalar(&tx, "SELECT reason FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3", params![root, binding.bucket, partner]).map_err(unavailable)?;
                        let marked = match stored {
                            Some(kind) if kind != FindingKind::Unmapped(Reason::Alias) => {
                                Marked::Other
                            }
                            Some(_) | None => {
                                write_alias(&tx, root, &binding, &partner, live, &spelling)?
                            }
                        };
                        Some((partner, marked))
                    }
                    None => None,
                };
                tx.commit().map_err(unavailable)?;
                Ok::<_, AdminError>(AliasRecorded { own, partner })
            })
            .await?;
        Ok(recorded)
    }

    /// Decides, before its file is read, whether `name`, live at `generation`, stays frozen: while
    /// another live object stands for the file its alias finding names or the file the tree
    /// tracks for it, whatever that file shows, since only an API change ends an alias. A held
    /// object's finding follows its live generation, and a tracked object whose file other
    /// objects' alias findings name gets one too. An alias that ended keeps its finding for the
    /// export that settles the file. Reads only while nothing changes.
    pub(crate) async fn hold_alias(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
    ) -> Result<AliasHold, RootError> {
        if !self.may_hold_aliases().await? {
            return Ok(AliasHold::NotAliased);
        }
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        let hold = self
            .catalog()
            .run_admin_with_error(move |c| {
                let tx = c.transaction().map_err(unavailable)?;
                lease.control_scope().authorize(&tx)?;
                matching(&tx, &binding, binding.bucket.as_str())?;
                let hold = decide_hold(&tx, root, &binding, &name, generation)?;
                tx.commit().map_err(unavailable)?;
                Ok::<_, AdminError>(hold)
            })
            .await?;
        Ok(hold)
    }

    /// Decides what the deletion of `name`, tracked at `path`, does to the file there, and
    /// removes the deleted object's findings when the file stays; see [`Holding`].
    pub(crate) async fn alias_holding(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        path: String,
    ) -> Result<Holding, RootError> {
        if !self.may_hold_aliases().await? {
            return Ok(Holding::Alone);
        }
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        let holding = self
            .catalog()
            .run_admin_with_error(move |c| {
                let tx = c.transaction().map_err(unavailable)?;
                lease.control_scope().authorize(&tx)?;
                matching(&tx, &binding, binding.bucket.as_str())?;
                if objects::lookup(&tx, binding.bucket.as_str(), name.as_str())?.is_some() {
                    return Err(AdminError::Precondition);
                }
                let left = claimants(&tx, root, &binding, &path, &name).map_err(unavailable)?;
                let edited: bool = sql::scalar(&tx, "SELECT EXISTS(SELECT 1 FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason=?5)", params![root, binding.bucket, name, binding.incarnation, PathStop::AliasEdited]).map_err(unavailable)?;
                let holding = match (left.len(), edited) {
                    (0, _) | (1, false) => Holding::Alone,
                    (1, true) => Holding::Edited,
                    _ => Holding::Shared,
                };
                if holding != Holding::Alone {
                    sql::exec(&tx, "DELETE FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason!=?5", params![root, binding.bucket, name, binding.incarnation, FindingKind::ExportConflict]).map_err(unavailable)?;
                }
                tx.commit().map_err(unavailable)?;
                Ok(holding)
            })
            .await?;
        Ok(holding)
    }

    /// The files of the tree that stand for more than one live object and stay frozen, by
    /// path, each with how many objects it stands for.
    pub(crate) async fn frozen_files(&self) -> Result<Vec<FrozenFile>, RootError> {
        if !self.may_hold_aliases().await? {
            return Ok(Vec::new());
        }
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |c| {
                let tx = c.transaction()?;
                lease.control_scope().authorize(&tx)?;
                tx.prepare_cached(FROZEN_FILES)?
                    .query_map([root], |row| {
                        Ok(FrozenFile {
                            path: row.get(0)?,
                            objects: row.get(1)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }

    /// Whether the tree tracks a live object of this root at `path`, which tells a file renamed
    /// to another object's spelling from one an alias holds under it.
    pub(crate) async fn tracked_live_at(&self, path: String) -> Result<bool, RootError> {
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |c| {
                let tx = c.transaction()?;
                lease.control_scope().authorize(&tx)?;
                sql::scalar(&tx, "SELECT EXISTS(SELECT 1 FROM sync_objects s JOIN sync_buckets b ON b.root_id=?1 AND b.bucket=s.bucket AND b.incarnation=s.incarnation JOIN objects o ON o.bucket=s.bucket AND o.name=s.name AND o.time_deleted IS NULL WHERE s.path=?2)", params![root, path])
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }

    /// The deleted object whose record holds the file that the alias finding of `name` names,
    /// which the file goes with or is taken in from before `name` can be settled.
    pub(crate) async fn deleted_alias_holder(
        &self,
        binding: SyncBinding,
        name: ObjectName,
    ) -> Result<Option<ObjectName>, RootError> {
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |c| {
                let tx = c.transaction()?;
                lease.control_scope().authorize(&tx)?;
                sql::optional_scalar(&tx, "SELECT s.name FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal JOIN sync_objects s ON s.bucket=f.bucket AND s.incarnation=f.incarnation AND s.path=a.spelling WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3 AND f.incarnation=?4 AND s.name!=?3 AND NOT EXISTS(SELECT 1 FROM objects o WHERE o.bucket=s.bucket AND o.name=s.name AND o.time_deleted IS NULL)", params![root, binding.bucket, name, binding.incarnation])
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }

    /// The spelling of the alias `name` belongs to, by its own finding or as the object the
    /// tree tracks at a spelling others name, read before a replay changes it: deleting the
    /// object may end the alias of the others.
    pub(crate) async fn alias_group(
        &self,
        bucket: BucketName,
        name: ObjectName,
    ) -> Result<Option<String>, RootError> {
        if !self.may_hold_aliases().await? {
            return Ok(None);
        }
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |c| {
                let tx = c.transaction()?;
                lease.control_scope().authorize(&tx)?;
                sql::optional_scalar(&tx, "SELECT a.spelling FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3
                    UNION SELECT s.path FROM sync_objects s WHERE s.bucket=?2 AND s.name=?3 AND EXISTS(SELECT 1 FROM sync_finding_aliases b JOIN sync_findings g ON g.ordinal=b.ordinal WHERE g.root_id=?1 AND g.bucket=?2 AND b.spelling=s.path) LIMIT 1", params![root, bucket, name])
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }

    /// The alias findings of `bucket` naming `spelling` after `after`, in ordinal order, a page
    /// at a time.
    pub(crate) async fn alias_findings_page(
        &self,
        bucket: BucketName,
        spelling: String,
        after: Ordinal,
    ) -> Result<Vec<NamedFinding>, RootError> {
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |c| {
                let tx = c.transaction()?;
                lease.control_scope().authorize(&tx)?;
                tx.prepare_cached("SELECT f.ordinal,f.name FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND a.spelling=?3 AND f.ordinal>?4 ORDER BY f.ordinal LIMIT 256")?
                    .query_map(params![root, bucket, spelling, after], |row| {
                        Ok(NamedFinding { ordinal: row.get(0)?, name: row.get(1)? })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }

    /// Records that the edited file at `path`, which the deleted object `holder` tracked, now
    /// holds what ingestion took in for the one object left of its alias: the record takes the
    /// file as `fingerprint` observed it, so that the deletion removes it unless it changed
    /// again, and the edit no longer stops the path.
    pub(crate) async fn hand_over_alias(
        &self,
        binding: SyncBinding,
        holder: ObjectName,
        path: SyncPath,
        fingerprint: SyncFingerprint,
    ) -> Result<(), RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin_with_error(move |c| {
                let tx = c.transaction().map_err(unavailable)?;
                lease.control_scope().authorize(&tx)?;
                matching(&tx, &binding, binding.bucket.as_str())?;
                if objects::lookup(&tx, binding.bucket.as_str(), holder.as_str())?.is_some() {
                    return Err(AdminError::Precondition);
                }
                let fp = &fingerprint;
                let updated = sql::exec(&tx, "UPDATE sync_objects SET size=?5,mtime_ns=?6,ctime_ns=?7,inode=?8,md5=?9,crc32c=?10,racy=?11 WHERE bucket=?1 AND name=?2 AND incarnation=?3 AND path=?4", params![binding.bucket, holder, binding.incarnation, path.as_str(), fp.size, fp.mtime_ns.to_be_bytes(), fp.ctime_ns.to_be_bytes(), fp.inode.to_be_bytes(), fp.md5, fp.crc32c, fp.racy]).map_err(unavailable)?;
                if updated != 1 {
                    return Err(AdminError::Precondition);
                }
                drop_edit_stop(&tx, root, &binding, &holder)?;
                tx.commit().map_err(unavailable)?;
                Ok::<_, AdminError>(())
            })
            .await?;
        Ok(())
    }

    /// Gives up taking the edit in the file of the deleted object `holder` in for the object
    /// left, which the API wrote since the edit was first seen: the edit no longer stops the
    /// path, so the deletion goes on to remove the file, which keeps the edit as a conflict, and
    /// the object left gets its own file with what the API holds.
    pub(crate) async fn abandon_alias_edit(
        &self,
        binding: SyncBinding,
        holder: ObjectName,
    ) -> Result<(), RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin_with_error(move |c| {
                let tx = c.transaction().map_err(unavailable)?;
                lease.control_scope().authorize(&tx)?;
                matching(&tx, &binding, binding.bucket.as_str())?;
                if objects::lookup(&tx, binding.bucket.as_str(), holder.as_str())?.is_some() {
                    return Err(AdminError::Precondition);
                }
                drop_edit_stop(&tx, root, &binding, &holder)?;
                tx.commit().map_err(unavailable)?;
                Ok::<_, AdminError>(())
            })
            .await?;
        Ok(())
    }
}

/// Removes the [`PathStop::AliasEdited`] stop of `holder`, whose edit is settled either way.
fn drop_edit_stop(
    transaction: &Transaction<'_>,
    root: RootId,
    binding: &SyncBinding,
    holder: &ObjectName,
) -> Result<(), AdminError> {
    sql::exec(transaction, "DELETE FROM sync_path_stops WHERE root_id=?1 AND bucket=?2 AND name=?3 AND incarnation=?4 AND reason=?5", params![root, binding.bucket, holder, binding.incarnation, PathStop::AliasEdited]).map_err(unavailable)?;
    Ok(())
}

/// [`RootLease::hold_alias`] inside its transaction.
fn decide_hold(
    transaction: &Transaction<'_>,
    root: RootId,
    binding: &SyncBinding,
    name: &ObjectName,
    generation: Generation,
) -> Result<AliasHold, AdminError> {
    let finding: Option<(Ordinal, Generation, String, bool)> = sql::row(transaction, "SELECT f.ordinal,f.generation,a.spelling,a.released FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3 AND f.incarnation=?4", params![root, binding.bucket, name, binding.incarnation], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).optional().map_err(unavailable)?;
    let tracked: Option<String> = sql::optional_scalar(
        transaction,
        "SELECT path FROM sync_objects WHERE bucket=?1 AND name=?2 AND incarnation=?3",
        params![binding.bucket, name, binding.incarnation],
    )
    .map_err(unavailable)?;
    let frozen = |spelling: &str| {
        claimants(transaction, root, binding, spelling, name)
            .map(|left| !left.is_empty())
            .map_err(unavailable)
    };
    let Some((ordinal, held, spelling, released)) = finding else {
        // A tracked file that other objects' alias findings name is theirs too, whatever this
        // object's finding said, such as a collision that cleared since.
        return Ok(match tracked {
            Some(path) if frozen(&path)? => {
                write_alias(transaction, root, binding, name, generation, &path)?;
                AliasHold::Held
            }
            Some(_) | None => AliasHold::NotAliased,
        });
    };
    let tracked_elsewhere = match tracked.as_deref() {
        Some(path) if path != spelling => frozen(path)?,
        Some(_) | None => false,
    };
    if frozen(&spelling)? || tracked_elsewhere {
        if held != generation {
            sql::exec(
                transaction,
                "UPDATE sync_findings SET generation=?2 WHERE ordinal=?1",
                params![ordinal, generation],
            )
            .map_err(unavailable)?;
        }
        if released {
            sql::exec(
                transaction,
                "UPDATE sync_finding_aliases SET released=0 WHERE ordinal=?1",
                [ordinal],
            )
            .map_err(unavailable)?;
        }
        return Ok(AliasHold::Held);
    }
    let transition = if released {
        Transition::Kept
    } else {
        sql::exec(
            transaction,
            "UPDATE sync_finding_aliases SET released=1 WHERE ordinal=?1",
            [ordinal],
        )
        .map_err(unavailable)?;
        Transition::New
    };
    Ok(AliasHold::Released {
        spelling,
        transition,
    })
}

/// Records the alias finding of `name`, live at `generation`, with `spelling`, and returns
/// what it did; an alias recorded again stands again, so a release it ended is withdrawn.
fn write_alias(
    transaction: &Transaction<'_>,
    root: RootId,
    binding: &SyncBinding,
    name: &ObjectName,
    generation: Generation,
    spelling: &str,
) -> Result<Marked, AdminError> {
    let alias = FindingKind::Unmapped(Reason::Alias);
    let written = write_name_finding(transaction, root, binding, name, generation, Reason::Alias)?;
    if written.after != alias {
        return Ok(Marked::Other);
    }
    let stored: Option<(String, bool)> = sql::row(transaction, "SELECT a.spelling,a.released FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3", params![root, binding.bucket, name], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(unavailable)?;
    if stored != Some((spelling.to_owned(), false)) {
        sql::exec(transaction, "INSERT INTO sync_finding_aliases(ordinal,spelling) SELECT ordinal,?4 FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 ON CONFLICT(ordinal) DO UPDATE SET spelling=excluded.spelling,released=0", params![root, binding.bucket, name, spelling]).map_err(unavailable)?;
    }
    Ok(if written.before == Some(alias) {
        Marked::Kept
    } else {
        Marked::New
    })
}
