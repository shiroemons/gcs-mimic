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
    storage::catalog::{
        acl, bucket_columns,
        bucket_update::{self, BucketChange, BucketWrite},
        change_journal, insert_bucket_with_policy,
        policies::{self, Metageneration, PolicyWrite},
        principals::Profile,
        read_bucket, sql, unavailable, unix_micros,
    },
};
use rusqlite::{
    Transaction, params,
    types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, ValueRef},
};
use std::{collections::HashMap, num::NonZeroU64};

#[cfg(test)]
mod tests;

/// The entry each bucket was created or last reconciled from, as a digest, so a later start can
/// tell an edited entry from an unchanged one. A row goes with its bucket through the foreign
/// key: deleting, soft deleting or resetting a bucket forgets its entry in the same transaction,
/// and a restored or recreated bucket starts without one.
pub(in crate::storage::catalog) const SCHEMA: &str = "CREATE TABLE bucket_file_entries (
    bucket TEXT PRIMARY KEY COLLATE BINARY REFERENCES buckets(name) ON DELETE CASCADE,
    digest BLOB NOT NULL CHECK(typeof(digest) = 'blob' AND length(digest) = 32)
) WITHOUT ROWID;
PRAGMA user_version = 52;";

const SELECT_EXISTING: &str = concat!(
    "SELECT ",
    bucket_columns!(),
    " FROM buckets WHERE name IN (SELECT value FROM json_each(?1))"
);
const PARENT_PREVENTS_PUBLIC_ACCESS: &str =
    "SELECT EXISTS(SELECT 1 FROM parent_public_access_prevention WHERE project = ?1)";
const SELECT_DIGEST: &str = "SELECT digest FROM bucket_file_entries WHERE bucket = ?1";
const RECORD_DIGEST: &str = "INSERT INTO bucket_file_entries VALUES (?1, ?2)
    ON CONFLICT(bucket) DO UPDATE SET digest = excluded.digest";

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
    /// `None` for a bucket that the API created, or that a release before the digests created.
    pub(crate) digest: Option<EntryDigest>,
}

/// What the run does for one target, in the order of the targets.
pub(crate) enum Planned {
    Create(Box<Creation>),
    /// Brings an existing bucket in line with its entry and records the entry.
    Reconcile(Box<Reconciliation>),
    /// Keeps the bucket and records the entry it now matches.
    Record(EntryDigest),
    Keep,
}

/// A missing bucket to create. A policy replaces the default one inside the same transaction.
pub(crate) struct Creation {
    pub(crate) definition: BucketDefinition,
    pub(crate) policy: Option<Policy>,
    pub(crate) digest: EntryDigest,
}

/// The writes that make an existing bucket hold what its entry writes. Each part writes only
/// what differs; the plan sets at least one of them.
pub(crate) struct Reconciliation {
    /// Applied as a PATCH applies it.
    pub(crate) update: Option<BucketMetadataUpdate>,
    /// Replaces the policy as setIamPolicy does.
    pub(crate) policy: Option<Policy>,
    pub(crate) digest: EntryDigest,
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
        let mut names = std::collections::HashSet::new();
        if targets.iter().any(|target| !names.insert(&target.name)) {
            return Err(AdminError::Invalid("duplicate bucket name".into()).into());
        }
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
    // The names are bound as one JSON array so the statement text stays static.
    let names = serde_json::to_string(
        &targets
            .iter()
            .map(|target| target.name.as_str())
            .collect::<Vec<_>>(),
    )
    .map_err(unavailable)?;
    let mut existing = transaction
        .prepare_cached(SELECT_EXISTING)?
        .query_map([names], read_bucket)?
        .map(|record| record.map(|record| (record.name.clone(), record)))
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
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
    Ok(Stored {
        policy: policies::read_policy(transaction, &record.name)?,
        digest: sql::optional_scalar(transaction, SELECT_DIGEST, [&record.name])?,
        record,
    })
}

impl Apply<'_> {
    fn step(&self, target: &InitTarget, step: Planned) -> Result<(), CatalogError> {
        match step {
            Planned::Create(creation) => self.create(target, *creation),
            Planned::Reconcile(reconciliation) => self.reconcile(&target.name, *reconciliation),
            Planned::Record(digest) => self.record(&target.name, digest),
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
            digest,
        }: Reconciliation,
    ) -> Result<(), CatalogError> {
        let metageneration = match update {
            Some(update) => {
                let change = BucketChange {
                    name: bucket,
                    update,
                    conditions: Default::default(),
                    access: &self.access,
                    mode: BucketWrite::Patch,
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
        self.record(bucket, digest)
    }

    fn record(&self, bucket: &str, digest: EntryDigest) -> Result<(), CatalogError> {
        sql::exec(self.transaction, RECORD_DIGEST, params![bucket, digest])?;
        Ok(())
    }

    fn create(
        &self,
        target: &InitTarget,
        Creation {
            definition,
            policy,
            digest,
        }: Creation,
    ) -> Result<(), CatalogError> {
        let mut record = definition.into_record();
        if record.name != target.name {
            return Err(unavailable("initialization plan names another bucket"));
        }
        let default_policy;
        let stored = match &policy {
            Some(policy) => policy,
            None => {
                default_policy = Policy::default_for(&record.project);
                &default_policy
            }
        };
        insert_bucket_with_policy(
            self.transaction,
            &mut record,
            self.default_retention,
            stored,
        )?;
        if let Some(policy) = &policy {
            // The plan has already refused such a policy; this keeps the catalog's own rule.
            if policy.names_public_principal()
                && policies::public_access_prevention(self.transaction, &record.name)?
            {
                return Err(CatalogError::PublicAccessPrevention);
            }
            // As a policy write does, so the bucket ACL shows the grants the policy makes.
            acl::replace_from_policy(self.transaction, &record.name, policy, self.principals)?;
        }
        change_journal::append(
            self.transaction,
            change_journal::origin(&self.access),
            change_journal::ChangeKind::BucketCreate,
            &record.name,
            None,
        )?;
        self.record(&record.name, digest)
    }
}
