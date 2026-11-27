//! Purging one bucket removes it with everything stored under its name, past holds and retention.
//!
//! The purge is one short immediate transaction on the writer connection, like an ordinary bucket
//! deletion. It takes no instance-wide gate: besides the connection it holds only the writers of
//! the bucket's own sessions, so a cleanup never makes other buckets' uploads wait. The rows go in
//! the transaction and their files after it, through the deletion journals that maintenance and
//! startup recovery also drain.

use super::{Admin, AdminError, BucketName};
use crate::storage::catalog::{
    Catalog, SessionId, appendable, bucket_soft_delete, objects, remove_live_bucket,
    remove_objects,
    resumable::{
        WriterGuard,
        writers::{ClaimError, KeyOrigin, WriterKey},
    },
    rewrite, sql,
};
use crate::{
    bucket::CatalogError,
    object::ObjectError,
    storage::{BlobError, BlobId, failure::object_unavailable},
};
use rusqlite::Connection;
use std::fmt;

#[cfg(test)]
mod tests;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PurgeOutcome {
    /// Every generation removed: live, noncurrent, soft-deleted and those of soft-deleted versions.
    objects: u64,
    /// Some file removal failed after commit and was left to maintenance or the next open.
    cleanup_pending: bool,
}

/// Why a purge made no changes, or could not confirm what it changed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PurgeError {
    /// No live or soft-deleted bucket has the name.
    NotFound,
    /// An import is incomplete.
    Importing,
    /// A request writes one of the bucket's upload sessions or appendable objects.
    WriterBusy,
    /// The administrative operation failed as any other does.
    Admin(#[source] AdminError),
}

// All purge failures share this diagnostic prefix, including the AdminError exposed as a source.
impl fmt::Display for PurgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bucket purge: ")?;
        match self {
            Self::NotFound => f.write_str("no live or soft-deleted bucket has the name"),
            Self::Importing => f.write_str("an import is incomplete"),
            Self::WriterBusy => f.write_str("a request writes one of the bucket's sessions"),
            Self::Admin(error) => error.fmt(f),
        }
    }
}

impl From<AdminError> for PurgeError {
    fn from(error: AdminError) -> Self {
        Self::Admin(error)
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    use std::error::Error;

    for (error, expected) in [
        (
            PurgeError::NotFound,
            "bucket purge: no live or soft-deleted bucket has the name",
        ),
        (
            PurgeError::Importing,
            "bucket purge: an import is incomplete",
        ),
        (
            PurgeError::WriterBusy,
            "bucket purge: a request writes one of the bucket's sessions",
        ),
    ] {
        assert_eq!(error.to_string(), expected);
        assert!(error.source().is_none());
        crate::error::assert_distinct_causes(&error);
    }
    let error = PurgeError::Admin(AdminError::Closed);
    assert_eq!(
        error.to_string(),
        "bucket purge: administrative operation: Closed"
    );
    assert!(error.source().unwrap().is::<AdminError>());
    crate::error::assert_distinct_causes(&error);
}

impl From<CatalogError> for PurgeError {
    fn from(error: CatalogError) -> Self {
        Self::Admin(error.into())
    }
}

impl From<ObjectError> for PurgeError {
    fn from(error: ObjectError) -> Self {
        Self::Admin(error.into())
    }
}

impl From<rusqlite::Error> for PurgeError {
    fn from(error: rusqlite::Error) -> Self {
        CatalogError::from(error).into()
    }
}

impl From<ClaimError> for PurgeError {
    fn from(error: ClaimError) -> Self {
        match error {
            ClaimError::Busy => Self::WriterBusy,
            error @ (ClaimError::Poisoned | ClaimError::Catalog(_)) => {
                CatalogError::from(error).into()
            }
        }
    }
}

impl Admin {
    #[cfg(test)]
    pub(crate) async fn purge_bucket(&self, name: &BucketName) -> Result<PurgeOutcome, PurgeError> {
        self.purge_bucket_until(
            name,
            crate::time_limit::TimeLimit::from_millis(60_000).deadline(),
        )
        .await
    }

    /// Purges the bucket, waiting for its turn on the writer connection until `deadline`; expiry
    /// returns `Busy` before anything changed.
    pub(crate) async fn purge_bucket_until(
        &self,
        name: &BucketName,
        deadline: tokio::time::Instant,
    ) -> Result<PurgeOutcome, PurgeError> {
        let catalog = self.catalog.clone();
        let name = name.as_str().to_owned();
        let purged = self
            .catalog
            .run_admin_until(deadline, move |connection| {
                purge(connection, &catalog, &name)
            })
            .await?;
        let objects = purged.objects;
        let cleanup_pending = self.catalog.release_purged(purged).await;
        Ok(PurgeOutcome {
            objects,
            cleanup_pending,
        })
    }
}

/// Whether the name still has a live bucket next to any soft-deleted versions.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Presence {
    Live,
    SoftDeletedOnly,
}

/// What a committed purge leaves for after its commit.
struct Purged {
    objects: u64,
    /// Kept until the files of the deleted sessions are gone.
    writers: Writers,
    files: SessionFiles,
    /// The deleted rewrite sessions, whose derived transfers may still pin source bytes.
    transfers: Vec<BlobId>,
}

/// The sessions stored under the bucket's name, in any incarnation of it.
struct Sessions {
    resumable: Vec<(SessionId, BlobId)>,
    multipart: Vec<String>,
    rewrite: Vec<BlobId>,
}

/// The writer of every session of the bucket, so no request writes one of them until the purge is
/// done with its rows and files.
struct Writers {
    _resumable: Vec<WriterGuard>,
    multipart: Vec<WriterGuard>,
    _rewrite: Vec<WriterGuard>,
}

/// Disk files of deleted rows that no deletion journal names. A RAM catalog journals the same data,
/// so it has none.
#[derive(Default)]
struct SessionFiles {
    staging: Vec<BlobId>,
    appendable: Vec<BlobId>,
}

const PRESENCE: &str = "SELECT EXISTS(SELECT 1 FROM buckets WHERE name = ?1), EXISTS(SELECT 1 FROM soft_deleted_buckets WHERE name = ?1)";
const RESUMABLE: &str = "SELECT id, blob_id FROM resumable_sessions WHERE bucket = ?1";
const MULTIPART: &str = "SELECT id FROM xml_multipart_uploads WHERE bucket = ?1";
/// Replays bind the bucket incarnation they wrote to, so this runs before the incarnation goes.
const REPLAYS: &str = "DELETE FROM upload_replays WHERE incarnation = (SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1)";
/// The publisher would otherwise deliver the bucket's queued events after it is gone. Each row
/// holds the Pub/Sub publish body, `{"messages":[..]}`, and every message names its bucket.
const OUTBOX: &str = "DELETE FROM notification_outbox WHERE EXISTS(SELECT 1 FROM json_each(message, '$.messages') WHERE json_extract(value, '$.attributes.bucketId') = ?1)";

/// The tables whose rows of the bucket the transaction deletes, by name, retained key or current
/// incarnation.
#[cfg(test)]
const DELETED: &[&str] = &[
    "appendable_objects",
    "bucket_incarnations",
    "bucket_policies",
    "buckets",
    "notification_outbox",
    "objects",
    "resumable_sessions",
    "rewrite_sessions",
    "seed_objects",
    "soft_deleted_buckets",
    "upload_replays",
    "xml_multipart_uploads",
];
/// The tables whose rows of the bucket go through foreign-key cascades.
#[cfg(test)]
const CASCADED: &[&str] = &[
    "acl_bucket_entries",
    "acl_object_entries",
    "bucket_file_entries",
    "bucket_notification_ids",
    "bucket_notifications",
    "xml_multipart_parts",
];
/// The tables whose rows keep the incarnation of a bucket they observed, which no later bucket of
/// the name shares: a seed bucket mark keeps a purged bucket from being created again while its
/// fixture directory stays.
#[cfg(test)]
const INCARNATION_BOUND: &[&str] = &["seed_buckets"];
/// The tables of the whole instance: allocators, the reset epoch, the deletion journals the purge
/// appends to, derived transfers owned by requests, and import state, which refuses the purge
/// while it would touch the bucket.
#[cfg(test)]
const INSTANCE_WIDE: &[&str] = &[
    "bucket_generation",
    "catalog_identity",
    "derived_deletions",
    "derived_inputs",
    "derived_transfers",
    "hmac_keys",
    "import_journal",
    "import_observations",
    "notification_outbox_state",
    "object_generation",
    "pending_blob_deletions",
    "policy_revision",
    "reset_epoch",
    "startup_seed",
    "xml_multipart_deletions",
];

fn purge(connection: &mut Connection, catalog: &Catalog, name: &str) -> Result<Purged, PurgeError> {
    let transaction = sql::immediate(connection)?;
    let presence = admit(&transaction, name)?;
    let sessions = Sessions::read(&transaction, name)?;
    let writers = sessions.claim(catalog, name)?;
    // Objects go first, so a completed session's blob shows no reference when it is journaled.
    let mut objects = bucket_soft_delete::remove_versions(&transaction, name)?;
    objects += remove_objects(&transaction, name)?;
    let files = sessions.remove(&transaction, catalog, name, &writers)?;
    for query in [REPLAYS, OUTBOX] {
        sql::exec(&transaction, query, [name])?;
    }
    if presence == Presence::Live {
        objects += remove_live_bucket(&transaction, name)?;
    }
    if let Err(error) = objects::commit_publication(transaction) {
        // A commit can succeed before reporting an I/O error; a name left with no version shows
        // that it did.
        let (live, deleted): (bool, bool) = sql::row(connection, PRESENCE, [name], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        if live || deleted {
            return Err(AdminError::from(error).into());
        }
    }
    Ok(Purged {
        objects,
        writers,
        files,
        transfers: sessions.rewrite,
    })
}

/// Whether the name has a live bucket. Refuses, changing nothing, a name with no version at all and
/// an incomplete import.
fn admit(transaction: &Connection, name: &str) -> Result<Presence, PurgeError> {
    let (live, deleted): (bool, bool) = sql::row(transaction, PRESENCE, [name], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?;
    let presence = match (live, deleted) {
        (true, _) => Presence::Live,
        (false, true) => Presence::SoftDeletedOnly,
        (false, false) => return Err(PurgeError::NotFound),
    };
    let importing: bool = sql::scalar(
        transaction,
        "SELECT EXISTS(SELECT 1 FROM import_journal WHERE active = 1)",
        [],
    )?;
    if importing {
        return Err(PurgeError::Importing);
    }
    Ok(presence)
}

impl Sessions {
    fn read(connection: &Connection, name: &str) -> Result<Self, PurgeError> {
        let resumable = connection
            .prepare_cached(RESUMABLE)?
            .query_map([name], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let multipart = connection
            .prepare_cached(MULTIPART)?
            .query_map([name], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self {
            resumable,
            multipart,
            rewrite: rewrite::bucket_sessions(connection, name)?,
        })
    }

    /// Claims every writer now or refuses: a request that holds one is writing, and waiting for it
    /// would hold the writer connection meanwhile.
    fn claim(&self, catalog: &Catalog, name: &str) -> Result<Writers, PurgeError> {
        if appendable::bucket_in_use(catalog, name) {
            return Err(PurgeError::WriterBusy);
        }
        let claim = |key| catalog.claim_writer(key, KeyOrigin::Holder);
        Ok(Writers {
            _resumable: self
                .resumable
                .iter()
                .map(|(id, _)| claim(WriterKey::Resumable(*id)))
                .collect::<Result<_, _>>()?,
            multipart: self
                .multipart
                .iter()
                .map(|id| claim(WriterKey::Multipart(id.clone())))
                .collect::<Result<_, _>>()?,
            _rewrite: self
                .rewrite
                .iter()
                .map(|id| claim(WriterKey::Rewrite(*id)))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Deletes the sessions and appendable objects of the bucket and returns the disk files that no
    /// journal names.
    fn remove(
        &self,
        transaction: &sql::WriteTx<'_>,
        catalog: &Catalog,
        name: &str,
        writers: &Writers,
    ) -> Result<SessionFiles, PurgeError> {
        sql::exec(
            transaction,
            "DELETE FROM resumable_sessions WHERE bucket = ?1",
            [name],
        )?;
        for (_, blob) in &self.resumable {
            // A completed session's blob belongs to the object it published while that object lives.
            if !objects::blob_referenced(transaction, blob)? {
                objects::journal_deletion(transaction, blob)?;
            }
        }
        for lease in &writers.multipart {
            Catalog::retire_held_multipart(transaction, lease)?;
        }
        for id in &self.rewrite {
            sql::exec(
                transaction,
                "DELETE FROM rewrite_sessions WHERE id = ?1",
                [id],
            )?;
        }
        let backend = catalog.0.storage.backend.kind();
        let appendable = appendable::retire_bucket(transaction, backend, name)?;
        Ok(match backend {
            crate::config::Backend::Memory => SessionFiles::default(),
            crate::config::Backend::Disk => SessionFiles {
                staging: self.resumable.iter().map(|(_, blob)| *blob).collect(),
                appendable,
            },
        })
    }
}

/// Whether some cleanup after commit failed. Each failure is logged; maintenance and the next open
/// retry whatever the journals still name.
#[derive(Default)]
struct Cleanup {
    pending: bool,
}

impl Cleanup {
    fn record<E: fmt::Display>(&mut self, step: &str, result: Result<(), E>) {
        if let Err(error) = result {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::BucketPurgeCleanupDeferred,
                serde_json::json!({"message": format!("{step}: {error}")}),
            );
            self.pending = true;
        }
    }
}

impl Catalog {
    /// Removes the files of a committed purge and reclaims the deletion journal, holding the
    /// bucket's writers until then. Returns whether any of it was left for later.
    async fn release_purged(&self, purged: Purged) -> bool {
        let Purged {
            writers,
            files,
            transfers,
            ..
        } = purged;
        let mut cleanup = Cleanup::default();
        cleanup.record("session files", self.remove_session_files(files).await);
        for id in transfers {
            let cancelled = match self.cancel_derived(&id).await {
                Err(ObjectError::DerivedTransferNotFound) => Ok(()),
                result => result,
            };
            cleanup.record("rewrite transfer", cancelled);
        }
        match self.drain_xml_multipart().await {
            // Parts left journaled wait for maintenance, which the drain has already woken.
            Ok(left) => cleanup.pending |= left,
            Err(error) => cleanup.record("multipart parts", Err(error)),
        }
        cleanup.record(
            "deletion journal",
            self.maintain_deletion_journal().await.map(drop),
        );
        drop(writers);
        if cleanup.pending {
            self.0.maintenance.wake.notify_one();
        }
        cleanup.pending
    }

    /// Removes the staging files of deleted resumable sessions and the data files of deleted
    /// appendable objects. Rows no longer name them, so startup recovery removes any left behind.
    async fn remove_session_files(&self, files: SessionFiles) -> Result<(), ObjectError> {
        if files.staging.is_empty() && files.appendable.is_empty() {
            return Ok(());
        }
        let backend = self.0.storage.backend.clone();
        crate::task::run_blocking("bucket-purge-cleanup", move || {
            for id in &files.staging {
                backend.remove_draft_durable(id)?;
            }
            for id in &files.appendable {
                backend.remove_appendable(id)?;
            }
            Ok::<_, BlobError>(())
        })
        .await
        .map_err(object_unavailable)?
        .map_err(ObjectError::from)
    }
}
