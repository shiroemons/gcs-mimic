//! Startup-only principal data excludes every bearer token and signing key.

use super::{Catalog, CatalogError, acl, unavailable, unix_micros};
use crate::auth::{Auth, Identity};
use crate::storage::catalog::sql;
use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(super) enum Profile {
    TrustedLocal,
    /// `creator` is the ACL entity recorded as owner of new objects; it grants nothing.
    Permissive {
        creator: Option<Arc<str>>,
    },
    Strict(Arc<BTreeMap<String, Identity>>),
}

impl Catalog {
    pub(super) fn set_principal_profile(&self, profile: Profile) -> Result<(), CatalogError> {
        self.0
            .principals
            .set(profile)
            .map_err(|_| CatalogError::AuthenticationAlreadyConfigured)
    }

    /// Installs the server's bounded trusted principals once, before admitting requests.
    pub async fn configure_authentication(&self, auth: &Auth) -> Result<(), CatalogError> {
        let profile = match auth.principals() {
            Some(identities) => Profile::Strict(Arc::new(
                identities
                    .iter()
                    .map(|identity| (identity.id().to_owned(), identity.clone()))
                    .collect(),
            )),
            None => Profile::Permissive {
                creator: auth.local_creator_entity().map(Arc::from),
            },
        };
        self.set_principal_profile(profile.clone())?;
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let (buckets, objects): (bool, bool) = connection
                .prepare_cached(
                    // Objects retained with a soft-deleted bucket version have no live bucket.
                    "SELECT EXISTS(SELECT 1 FROM buckets), EXISTS(SELECT 1 FROM objects WHERE NOT EXISTS(SELECT 1 FROM soft_deleted_buckets s WHERE objects.bucket = s.name || '#' || s.generation))",
                )
                .and_then(|mut statement| {
                    statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
                })
                .map_err(unavailable)?;
            if !buckets {
                if objects {
                    return Err(CatalogError::Corrupt(
                        "object records exist without a bucket".into(),
                    ));
                }
                return Ok(());
            }
            let mut after: Option<String> = None;
            loop {
                let transaction = sql::immediate(connection)
                    .map_err(unavailable)?;
                let query = if after.is_some() {
                    "SELECT name FROM buckets WHERE name>?1 ORDER BY name LIMIT 1"
                } else {
                    "SELECT name FROM buckets WHERE ?1 IS NULL ORDER BY name LIMIT 1"
                };
                let bucket: Option<String> = sql::optional_scalar(&transaction, query, [&after])
                    .map_err(unavailable)?;
                let Some(bucket) = bucket else { break };
                acl::reconcile_bucket_policy(
                    &transaction,
                    &bucket,
                    &profile,
                    unix_micros(clock.now())?,
                )?;
                transaction.commit().map_err(unavailable)?;
                after = Some(bucket);
            }
            super::object_owner::reconcile(connection, &profile)
        })
        .await
    }
}

impl Profile {
    pub(super) fn canonical_acl_entity(&self, id: &str) -> Option<String> {
        if let Self::Strict(identities) = self
            && let Some(identity) = identities.get(id)
        {
            return identity.acl_entity();
        }
        let entity = format!("user-{id}");
        matches!(
            crate::acl::Entity::parse(&entity),
            Ok(crate::acl::Entity::UserEmail(_) | crate::acl::Entity::UserId(_))
        )
        .then_some(entity)
    }
}
