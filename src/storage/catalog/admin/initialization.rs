//! The catalog side of the bucket initialization file.
//!
//! One administrative transaction observes what the file's bucket names hold, hands that state
//! to the initializer's plan and applies the plan. A plan that rejects the file returns before any
//! statement writes, so a rejected file changes nothing, and stored state cannot change between
//! the observation a plan relies on and the writes it asks for.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{Admin, AdminError, AdminGrant, BucketDefinition, Origin};
use crate::{
    auth::{Policy, PublicAccess},
    bucket::{BucketRecord, CatalogError},
    storage::catalog::{
        acl, bucket_columns, change_journal, insert_bucket_with_policy, policies,
        principals::Profile, read_bucket, sql, unavailable,
    },
};
use rusqlite::Transaction;
use std::{collections::HashMap, num::NonZeroU64};

const SELECT_EXISTING: &str = concat!(
    "SELECT ",
    bucket_columns!(),
    " FROM buckets WHERE name IN (SELECT value FROM json_each(?1))"
);
const PARENT_PREVENTS_PUBLIC_ACCESS: &str =
    "SELECT EXISTS(SELECT 1 FROM parent_public_access_prevention WHERE project = ?1)";

/// The bucket one file entry names and the project that would own it if it were created.
pub(crate) struct InitTarget {
    pub(crate) name: String,
    pub(crate) project: String,
}

/// What the catalog holds for one target when the run starts.
pub(crate) struct Observed {
    pub(crate) existing: Option<BucketRecord>,
    /// The parent project's public access prevention: the stored owner's for an existing
    /// bucket, the target project's for a missing one.
    pub(crate) parent: PublicAccess,
}

/// What the run does for one target, in the order of the targets.
pub(crate) enum Planned {
    Create(Box<Creation>),
    Keep,
}

/// A missing bucket to create. A policy replaces the default one inside the same transaction.
pub(crate) struct Creation {
    pub(crate) definition: BucketDefinition,
    pub(crate) policy: Option<Policy>,
}

/// What every applied step shares.
struct Apply<'a> {
    transaction: &'a Transaction<'a>,
    origin: Origin,
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
                    origin: grant.origin(),
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
            let existing = existing.remove(&target.name);
            let project = existing
                .as_ref()
                .map_or(target.project.as_str(), |record| record.project.as_str());
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

impl Apply<'_> {
    fn step(&self, target: &InitTarget, step: Planned) -> Result<(), CatalogError> {
        match step {
            Planned::Create(creation) => self.create(target, *creation),
            Planned::Keep => Ok(()),
        }
    }

    fn create(
        &self,
        target: &InitTarget,
        Creation { definition, policy }: Creation,
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
            self.origin,
            change_journal::ChangeKind::BucketCreate,
            &record.name,
            None,
        )
    }
}
