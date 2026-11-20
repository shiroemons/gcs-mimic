//! The spelling under which the tree holds the one file of objects whose names the root's file
//! system takes for the same name, beside their `alias` findings. The file stands for every
//! object of the alias while more than one of them lives, so none of them is exported over it,
//! ingested from it or deleted because it went; it is released once an API change leaves one.
use super::*;
use crate::storage::{admin::BucketName, sync_state::observation::Transition};

/// The schema that keeps an alias finding's spelling. The number is written only here:
/// [`migrate`] stores it, so that reordering it against another lane's schema changes one line.
pub(in crate::storage) const SCHEMA_VERSION: i64 = 62;
/// A spelling goes with its finding, and with the `alias` reason once the finding gets another
/// one; a spelling that changes moves the findings revision as the finding itself does.
pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_finding_aliases (
 ordinal INTEGER PRIMARY KEY REFERENCES sync_findings(ordinal) ON DELETE CASCADE,
 spelling TEXT NOT NULL COLLATE BINARY CHECK(length(spelling) BETWEEN 1 AND 4096)
);
CREATE INDEX sync_finding_aliases_spelling ON sync_finding_aliases(spelling);
CREATE TRIGGER sync_finding_alias_reason AFTER UPDATE OF reason ON sync_findings WHEN NEW.reason!='alias' BEGIN
 DELETE FROM sync_finding_aliases WHERE ordinal=NEW.ordinal;
END;
CREATE TRIGGER sync_observation_finding_alias_update AFTER UPDATE ON sync_finding_aliases BEGIN
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

/// What recording an alias did to the objects it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AliasRecorded {
    /// Whether this object's finding became an alias finding now. `None` when the finding kept
    /// another reason, as an export conflict of the same incarnation does.
    pub own: Option<Transition>,
    /// The other object the tree tracks at the spelling, when the API still holds it, with
    /// whether its finding became an alias finding now.
    pub partner: Option<(ObjectName, Option<Transition>)>,
}

/// Whether an object whose path the tree does not hold under another spelling stays frozen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AliasHold {
    /// The object has no alias finding.
    NotAliased,
    /// Another object of the alias still lives, so the file stays frozen.
    Held,
    /// The alias ended now, and the finding that named `spelling` went.
    Released { spelling: String },
}

/// The ordinal and name of an alias finding, by which a replay observes the findings of a
/// bucket again.
pub(crate) struct AliasFinding {
    pub ordinal: Ordinal,
    pub name: ObjectName,
}

impl RootLease {
    /// Records `name`, live at `generation`, as an alias whose file the tree holds at
    /// `spelling`, and the object tracked at `spelling` too while the API holds it, at its live
    /// generation. Each finding follows [`RootLease::record_finding`], and recording what the
    /// rows already say only reads.
    pub(crate) async fn record_alias(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
        spelling: String,
    ) -> Result<AliasRecorded, RootError> {
        self.revalidate().await?;
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
                        let written = write_alias(&tx, root, &binding, &partner, live, &spelling)?;
                        Some((partner, written))
                    }
                    None => None,
                };
                tx.commit().map_err(unavailable)?;
                Ok::<_, AdminError>(AliasRecorded { own, partner })
            })
            .await?;
        Ok(recorded)
    }

    /// Decides whether `name`, live at `generation`, whose path the tree does not hold under
    /// another spelling now, stays frozen by its alias finding: while another object whose alias
    /// finding names the same spelling still lives, whatever the file shows, since only an API
    /// change ends an alias. The held finding follows the object's live generation. Otherwise
    /// its finding goes, and the caller exports the object as any other; an edit the freeze
    /// kept out stays stopped for ingestion to take in. Reads only while nothing changes.
    pub(crate) async fn hold_alias(
        &self,
        binding: SyncBinding,
        name: ObjectName,
        generation: Generation,
    ) -> Result<AliasHold, RootError> {
        self.revalidate().await?;
        let lease = self.clone();
        let root = self.status().root_id;
        let hold = self
            .catalog()
            .run_admin(move |c| {
                let tx = c.transaction()?;
                lease.control_scope().authorize(&tx)?;
                matching(&tx, &binding, binding.bucket.as_str()).map_err(unavailable)?;
                let Some((ordinal, held, spelling)) = sql::row(&tx, "SELECT f.ordinal,f.generation,a.spelling FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3 AND f.incarnation=?4", params![root, binding.bucket, name, binding.incarnation], |row| Ok((row.get::<_, Ordinal>(0)?, row.get::<_, Generation>(1)?, row.get::<_, String>(2)?))).optional()? else {
                    return Ok(AliasHold::NotAliased);
                };
                let partnered: bool = sql::scalar(&tx, "SELECT EXISTS(SELECT 1 FROM sync_finding_aliases a JOIN sync_findings f ON f.ordinal=a.ordinal JOIN objects o ON o.bucket=f.bucket AND o.name=f.name AND o.time_deleted IS NULL WHERE a.spelling=?4 AND f.root_id=?1 AND f.bucket=?2 AND f.name!=?3)", params![root, binding.bucket, name, spelling])?;
                let hold = if partnered {
                    if held != generation {
                        sql::exec(&tx, "UPDATE sync_findings SET generation=?2 WHERE ordinal=?1", params![ordinal, generation])?;
                    }
                    AliasHold::Held
                } else {
                    sql::exec(&tx, "DELETE FROM sync_findings WHERE ordinal=?1", [ordinal])?;
                    AliasHold::Released { spelling }
                };
                tx.commit().map_err(unavailable)?;
                Ok(hold)
            })
            .await?;
        Ok(hold)
    }

    /// The alias findings of `bucket` after `after`, in ordinal order, a page at a time.
    pub(crate) async fn alias_findings_page(
        &self,
        bucket: BucketName,
        after: Ordinal,
    ) -> Result<Vec<AliasFinding>, RootError> {
        let lease = self.clone();
        let root = self.status().root_id;
        self.catalog()
            .run_admin(move |c| {
                let tx = c.transaction()?;
                lease.control_scope().authorize(&tx)?;
                tx.prepare_cached("SELECT f.ordinal,f.name FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND f.ordinal>?3 ORDER BY f.ordinal LIMIT 256")?
                    .query_map(params![root, bucket, after], |row| {
                        Ok(AliasFinding { ordinal: row.get(0)?, name: row.get(1)? })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(unavailable)
            })
            .await
            .map_err(RootError::from)
    }
}

/// Records the alias finding of `name`, live at `generation`, with `spelling`, and returns
/// whether it began now; `None` when the finding kept another reason.
fn write_alias(
    transaction: &Transaction<'_>,
    root: RootId,
    binding: &SyncBinding,
    name: &ObjectName,
    generation: Generation,
    spelling: &str,
) -> Result<Option<Transition>, AdminError> {
    let alias = FindingKind::Unmapped(Reason::Alias);
    let written = write_name_finding(transaction, root, binding, name, generation, Reason::Alias)?;
    if written.after != alias {
        return Ok(None);
    }
    let stored: Option<String> = sql::optional_scalar(transaction, "SELECT a.spelling FROM sync_findings f JOIN sync_finding_aliases a ON a.ordinal=f.ordinal WHERE f.root_id=?1 AND f.bucket=?2 AND f.name=?3", params![root, binding.bucket, name]).map_err(unavailable)?;
    if stored.as_deref() != Some(spelling) {
        sql::exec(transaction, "INSERT INTO sync_finding_aliases(ordinal,spelling) SELECT ordinal,?4 FROM sync_findings WHERE root_id=?1 AND bucket=?2 AND name=?3 ON CONFLICT(ordinal) DO UPDATE SET spelling=excluded.spelling", params![root, binding.bucket, name, spelling]).map_err(unavailable)?;
    }
    Ok(Some(if written.before == Some(alias) {
        Transition::Kept
    } else {
        Transition::New
    }))
}
