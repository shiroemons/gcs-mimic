//! Ownership joins the existing metadata publication transaction, without new blob steps.

use super::{objects::object_unavailable, principals::Profile, unavailable};
use crate::{
    bucket::{BucketAccess, CatalogError},
    object::{ObjectError, owner::ObjectOwner},
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, types::Type};

pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN owner TEXT CHECK(owner IS NULL OR (json_valid(owner) AND json_type(owner) = 'object')); PRAGMA user_version=44;";

#[cfg(test)]
mod tests;

pub(super) fn canonicalize(owner: &mut ObjectOwner, profile: Option<&Profile>) {
    match profile {
        Some(Profile::Strict(identities)) => {
            if let ObjectOwner::Principal { id, entity } = owner {
                *entity = identities
                    .get(id)
                    .and_then(crate::auth::Identity::acl_entity);
            }
        }
        Some(Profile::Permissive | Profile::TrustedLocal) | None => {}
    }
}

pub(super) fn reconcile(
    connection: &mut Connection,
    profile: &Profile,
) -> Result<(), CatalogError> {
    if !matches!(profile, Profile::Strict(_)) {
        return Ok(());
    }
    let mut after: Option<(String, String, i64)> = None;
    loop {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(unavailable)?;
        let query = if after.is_some() {
            "SELECT bucket,name,generation,owner FROM objects WHERE owner IS NOT NULL AND (bucket,name,generation)>(?1,?2,?3) ORDER BY bucket,name,generation LIMIT 1"
        } else {
            "SELECT bucket,name,generation,owner FROM objects WHERE owner IS NOT NULL AND ?1 IS NULL AND ?2 IS NULL AND ?3 IS NULL ORDER BY bucket,name,generation LIMIT 1"
        };
        let row = transaction
            .prepare_cached(query)
            .and_then(|mut statement| {
                statement.query_row(
                    params![
                        after.as_ref().map(|key| &key.0),
                        after.as_ref().map(|key| &key.1),
                        after.as_ref().map(|key| key.2)
                    ],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            read(row, 3)?,
                        ))
                    },
                )
            })
            .optional()
            .map_err(unavailable)?;
        let Some((bucket, name, generation, Some(mut owner))) = row else {
            return Ok(());
        };
        let before = owner.clone();
        canonicalize(&mut owner, Some(profile));
        if owner != before {
            let owner = serde_json::to_string(&owner).map_err(unavailable)?;
            transaction
                .prepare_cached(
                    "UPDATE objects SET owner=?4 WHERE bucket=?1 AND name=?2 AND generation=?3",
                )
                .and_then(|mut statement| {
                    statement.execute(params![bucket, name, generation, owner])
                })
                .map_err(unavailable)?;
        }
        transaction.commit().map_err(unavailable)?;
        after = Some((bucket, name, generation));
    }
}

pub(super) fn for_publication(
    connection: &Connection,
    bucket: &str,
    access: &BucketAccess,
) -> Result<ObjectOwner, ObjectError> {
    if let BucketAccess::Policy {
        identity: Some(identity),
        ..
    } = access
    {
        return Ok(ObjectOwner::Principal {
            id: identity.id().to_owned(),
            entity: identity.acl_entity(),
        });
    }
    let project = connection
        .prepare_cached("SELECT project FROM buckets WHERE name=?1")
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
        .map_err(object_unavailable)?;
    Ok(ObjectOwner::ProjectOwners { project })
}

pub(super) fn read(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<ObjectOwner>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
            })
        })
        .transpose()
}

pub(super) fn selected(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<Option<ObjectOwner>, CatalogError> {
    connection
        .prepare_cached("SELECT owner FROM objects WHERE bucket=?1 AND name=?2 AND generation=?3")
        .and_then(|mut statement| {
            statement.query_row(rusqlite::params![bucket, name, generation], |row| {
                read(row, 0)
            })
        })
        .optional()
        .map(Option::flatten)
        .map_err(unavailable)
}
