//! Startup-only principal data excludes every bearer token and signing key.

use super::{Catalog, CatalogError, acl, unix_micros};
use crate::acl::Entity;
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
        creator: Option<Entity>,
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
                creator: auth.local_creator_entity().cloned(),
            },
        };
        self.set_principal_profile(profile.clone())?;
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            // Objects retained with a soft-deleted bucket version have no live bucket.
            const SELECT: &str = "SELECT EXISTS(SELECT 1 FROM buckets), EXISTS(SELECT 1 FROM objects WHERE NOT EXISTS(SELECT 1 FROM soft_deleted_buckets s WHERE objects.bucket = s.name || '#' || s.generation))";
            let (buckets, objects): (bool, bool) = sql::row(connection, SELECT, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
            if !buckets {
                if objects {
                    return Err(CatalogError::Corrupt(
                        "object records exist without a bucket".into(),
                    ));
                }
                return Ok(());
            }
            let mut after = super::After::Start;
            loop {
                let transaction = sql::immediate(connection)?;
                let bucket: Option<String> = sql::optional_scalar(
                    &transaction,
                    "SELECT name FROM buckets WHERE name>?1 ORDER BY name LIMIT 1",
                    [&after],
                )?;
                let Some(bucket) = bucket else { break };
                acl::reconcile_bucket_policy(
                    &transaction,
                    &bucket,
                    &profile,
                    unix_micros(clock.now())?,
                )?;
                transaction.commit()?;
                after = super::After::Key(bucket);
            }
            super::object_owner::reconcile(connection, &profile)
        })
        .await
    }
}

impl Profile {
    /// A registered identity's entity, or else the same natural derivation an identity uses.
    pub(super) fn canonical_acl_entity(&self, id: &str) -> Option<Entity> {
        if let Self::Strict(identities) = self
            && let Some(identity) = identities.get(id)
        {
            return identity.acl_entity();
        }
        Entity::from_user_id(id)
    }
}
