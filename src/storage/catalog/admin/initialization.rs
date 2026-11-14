//! The catalog side of the bucket initialization file.
//!
//! One administrative transaction observes what the file's bucket names hold, hands that state
//! to the initializer's plan and applies the plan. A plan that rejects the file returns before any
//! statement writes, so a rejected file changes nothing, and stored state cannot change between
//! the observation a plan relies on and the writes it asks for.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{Admin, AdminError, AdminGrant, BucketDefinition};
use crate::{
    auth::{Policy, PublicAccess},
    bucket::{BucketAccess, BucketMetadataUpdate, BucketRecord, CatalogError},
    clock::Clock,
    object::WriteMode,
    storage::catalog::{
        BucketCreation,
        bucket_update::{self, BucketChange},
        change_journal, create_bucket_in_transaction,
        policies::{self, Metageneration, PolicyWrite},
        principals::Profile,
        sql, unavailable, unix_micros,
    },
};
use rusqlite::{
    OptionalExtension, Transaction, params,
    types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef},
};
use std::num::NonZeroU64;

#[cfg(test)]
mod tests;

/// What each bucket's file entry was when a start last recorded it, so a later start can tell an
/// edited entry from an unchanged one and a member the entry stopped writing. `digest` is the
/// entry the bucket was created, last reconciled or found to match; `members` is the set of
/// members the latest recorded entry wrote. A row goes with its bucket through the foreign key:
/// deleting, soft deleting or resetting a bucket forgets its entry in the same transaction, and a
/// restored or recreated bucket starts without one.
pub(in crate::storage::catalog) const SCHEMA: &str = "CREATE TABLE bucket_file_entries (
    bucket TEXT PRIMARY KEY COLLATE BINARY REFERENCES buckets(name) ON DELETE CASCADE,
    digest BLOB NOT NULL CHECK(typeof(digest) = 'blob' AND length(digest) = 32),
    members INTEGER NOT NULL CHECK(typeof(members) = 'integer' AND members BETWEEN 0 AND 4294967295)
) WITHOUT ROWID;
PRAGMA user_version = 52;";

const PARENT_PREVENTS_PUBLIC_ACCESS: &str =
    "SELECT EXISTS(SELECT 1 FROM parent_public_access_prevention WHERE project = ?1)";
const SELECT_ENTRY: &str = "SELECT digest, members FROM bucket_file_entries WHERE bucket = ?1";
const RECORD_ENTRY: &str = "INSERT INTO bucket_file_entries VALUES (?1, ?2, ?3)
    ON CONFLICT(bucket) DO UPDATE SET digest = excluded.digest, members = excluded.members";
const RECORD_MEMBERS: &str = "UPDATE bucket_file_entries SET members = ?2 WHERE bucket = ?1";

/// The SHA-256 digest of one normalized bucket file entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryDigest([u8; 32]);

impl EntryDigest {
    pub(crate) const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl ToSql for EntryDigest {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(ValueRef::Blob(&self.0)))
    }
}

impl FromSql for EntryDigest {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        <[u8; 32]>::column_result(value).map(Self)
    }
}

/// The members an entry wrote, one bit each. The initializer owns what each bit means; the
/// catalog only keeps the bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MemberBits(u32);

impl MemberBits {
    pub(crate) const fn new(bits: u32) -> Self {
        Self(bits)
    }

    pub(crate) const fn get(self) -> u32 {
        self.0
    }
}

impl ToSql for MemberBits {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl FromSql for MemberBits {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        u32::column_result(value).map(Self)
    }
}

/// One recorded file entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecordedEntry {
    pub(crate) digest: EntryDigest,
    pub(crate) members: MemberBits,
}

/// How a run changes the entry recorded for an existing bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EntryUpdate {
    /// The bucket now holds what the entry writes: the entry becomes the recorded one.
    Matched(RecordedEntry),
    /// The bucket still differs from the entry, so the recorded digest stays and the difference
    /// is reported again; only which members the entry writes is recorded, so that a removed
    /// member is reported once. A bucket without a recorded entry keeps none.
    Members(MemberBits),
}

/// The bucket one file entry names and the project that would own it if it were created.
pub(crate) struct InitTarget {
    pub(crate) name: String,
    pub(crate) project: String,
}

/// What the catalog holds for one target when the run starts.
pub(crate) struct Observed {
    pub(crate) existing: Option<Stored>,
    /// The parent project's public access prevention: the stored owner's for an existing
    /// bucket, the target project's for a missing one.
    pub(crate) parent: PublicAccess,
}

/// An existing bucket as the catalog holds it.
pub(crate) struct Stored {
    pub(crate) record: BucketRecord,
    pub(crate) policy: Policy,
    /// `None` for a bucket that the API created, or that a release before the records created.
    pub(crate) entry: Option<RecordedEntry>,
}

/// What the run does for one target, in the order of the targets.
pub(crate) enum Planned {
    Create(Box<Creation>),
    /// Brings an existing bucket closer to its entry.
    Reconcile(Box<Reconciliation>),
    /// Keeps the bucket and changes its recorded entry.
    Record(EntryUpdate),
    Keep,
}

/// A missing bucket to create. A policy replaces the default one inside the same transaction.
pub(crate) struct Creation {
    pub(crate) definition: BucketDefinition,
    pub(crate) policy: Option<Policy>,
    pub(crate) entry: RecordedEntry,
}

/// The writes that bring an existing bucket closer to what its entry writes. Each part writes
/// only what differs; the plan sets `update` or `policy`.
pub(crate) struct Reconciliation {
    /// Applied as a PATCH applies it.
    pub(crate) update: Option<BucketMetadataUpdate>,
    /// Replaces the policy as setIamPolicy does.
    pub(crate) policy: Option<Policy>,
    /// `None` keeps the recorded entry as it is.
    pub(crate) entry: Option<EntryUpdate>,
}

/// What every applied step shares.
struct Apply<'a> {
    transaction: &'a Transaction<'a>,
    /// The local administrative authority, whose origin every journal entry names.
    access: BucketAccess,
    clock: &'a dyn Clock,
    default_retention: Option<NonZeroU64>,
    principals: Option<&'a Profile>,
}

impl Admin {
    /// Observes every target, lets `plan` decide from that state, then applies its steps and
    /// commits, all in one transaction. An error from `plan` leaves the catalog unchanged.
    pub(crate) async fn initialize_buckets<T, E>(
        &self,
        targets: Vec<InitTarget>,
        plan: impl FnOnce(Vec<Observed>) -> Result<(Vec<Planned>, T), E> + Send + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<AdminError> + Send + 'static,
    {
        super::require_unique_names(targets.iter().map(|target| target.name.as_str()))?;
        let grant = self.grant.clone();
        let clock = self.catalog.0.clock.clone();
        let default_retention = self.catalog.default_soft_delete_retention();
        let principals = self.catalog.0.principals.get().cloned();
        self.catalog
            .run_admin_with_error(move |connection| {
                let failed = |error: CatalogError| E::from(AdminError::from(error));
                let transaction = sql::immediate(connection).map_err(|e| failed(e.into()))?;
                let observed = observe(&transaction, &grant, &targets).map_err(failed)?;
                let (planned, outcome) = plan(observed)?;
                if planned.len() != targets.len() {
                    return Err(AdminError::Invalid("incomplete initialization plan".into()).into());
                }
                let apply = Apply {
                    transaction: &transaction,
                    access: BucketAccess::Admin(grant),
                    clock: clock.as_ref(),
                    default_retention,
                    principals: principals.as_ref(),
                };
                for (target, step) in targets.iter().zip(planned) {
                    apply.step(target, step).map_err(failed)?;
                }
                transaction.commit().map_err(|e| failed(e.into()))?;
                Ok(outcome)
            })
            .await
    }
}

fn observe(
    transaction: &Transaction<'_>,
    grant: &AdminGrant,
    targets: &[InitTarget],
) -> Result<Vec<Observed>, CatalogError> {
    grant.authorize(transaction)?;
    let mut existing = super::existing_buckets(
        transaction,
        targets.iter().map(|target| target.name.as_str()),
    )?;
    targets
        .iter()
        .map(|target| {
            let existing = existing
                .remove(&target.name)
                .map(|record| stored(transaction, record))
                .transpose()?;
            let project = existing.as_ref().map_or(target.project.as_str(), |stored| {
                stored.record.project.as_str()
            });
            let prevented: bool =
                sql::scalar(transaction, PARENT_PREVENTS_PUBLIC_ACCESS, [project])?;
            Ok(Observed {
                existing,
                parent: if prevented {
                    PublicAccess::Prevented
                } else {
                    PublicAccess::Allowed
                },
            })
        })
        .collect()
}

fn stored(transaction: &Transaction<'_>, record: BucketRecord) -> Result<Stored, CatalogError> {
    let entry = sql::row(transaction, SELECT_ENTRY, [&record.name], |row| {
        Ok(RecordedEntry {
            digest: row.get(0)?,
            members: row.get(1)?,
        })
    })
    .optional()?;
    Ok(Stored {
        policy: policies::read_policy(transaction, &record.name)?,
        entry,
        record,
    })
}

impl Apply<'_> {
    fn step(&self, target: &InitTarget, step: Planned) -> Result<(), CatalogError> {
        match step {
            Planned::Create(creation) => self.create(target, *creation),
            Planned::Reconcile(reconciliation) => self.reconcile(&target.name, *reconciliation),
            Planned::Record(update) => self.update_entry(&target.name, update),
            Planned::Keep => Ok(()),
        }
    }

    /// Writes through the same steps as a PATCH and a setIamPolicy, journal entries included. A
    /// bucket that changes in both advances its metageneration once, as one change.
    fn reconcile(
        &self,
        bucket: &str,
        Reconciliation {
            update,
            policy,
            entry,
        }: Reconciliation,
    ) -> Result<(), CatalogError> {
        let metageneration = match update {
            Some(update) => {
                let change = BucketChange {
                    name: bucket,
                    update,
                    conditions: Default::default(),
                    access: &self.access,
                    mode: WriteMode::Patch,
                };
                bucket_update::update_in_transaction(
                    self.transaction,
                    change,
                    self.clock,
                    self.principals,
                )?;
                Metageneration::Advanced
            }
            None => Metageneration::Advance {
                updated: unix_micros(self.clock.now())?,
            },
        };
        if let Some(mut policy) = policy {
            let write = PolicyWrite {
                bucket,
                access: &self.access,
                metageneration,
            };
            policies::replace_policy(self.transaction, write, &mut policy, self.principals)?;
        }
        entry.map_or(Ok(()), |entry| self.update_entry(bucket, entry))
    }

    fn update_entry(&self, bucket: &str, update: EntryUpdate) -> Result<(), CatalogError> {
        match update {
            EntryUpdate::Matched(entry) => self.record(bucket, entry),
            EntryUpdate::Members(members) => {
                sql::exec(self.transaction, RECORD_MEMBERS, params![bucket, members])?;
                Ok(())
            }
        }
    }

    fn record(&self, bucket: &str, entry: RecordedEntry) -> Result<(), CatalogError> {
        sql::exec(
            self.transaction,
            RECORD_ENTRY,
            params![bucket, entry.digest, entry.members],
        )?;
        Ok(())
    }

    fn create(
        &self,
        target: &InitTarget,
        Creation {
            definition,
            policy,
            entry,
        }: Creation,
    ) -> Result<(), CatalogError> {
        let mut record = definition.into_record();
        if record.name != target.name {
            return Err(unavailable("initialization plan names another bucket"));
        }
        // The plan has already refused a public policy under public access prevention; the
        // creation keeps the catalog's own rule as well.
        let creation = BucketCreation {
            origin: change_journal::origin(&self.access),
            default_retention: self.default_retention,
            policy: policy.as_ref(),
            principals: self.principals,
        };
        create_bucket_in_transaction(self.transaction, &mut record, creation)?;
        self.record(&record.name, entry)
    }
}
