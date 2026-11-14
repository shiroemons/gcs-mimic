//! Ownership joins the existing metadata publication transaction, without new blob steps.

use super::{principals::Profile, unavailable};
use crate::storage::catalog::sql;
use crate::{
    bucket::{BucketAccess, CatalogError},
    object::{
        ObjectError,
        owner::{LOCAL_CREATOR_ID, ObjectOwner},
    },
};
use rusqlite::{Connection, OptionalExtension, Row, params};

pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN owner TEXT CHECK(owner IS NULL OR (json_valid(owner) AND json_type(owner) = 'object')); PRAGMA user_version=44;";

#[cfg(test)]
mod tests;

pub(super) fn canonicalize(owner: &mut ObjectOwner, profile: Option<&Profile>) {
    match profile {
        Some(Profile::Strict(identities)) => {
            // A strict restart keeps the recorded creator of objects written while permissive.
            if let ObjectOwner::Principal { id, entity } = owner
                && id != LOCAL_CREATOR_ID
            {
                *entity = identities
                    .get(id)
                    .and_then(crate::auth::Identity::acl_entity);
            }
        }
        Some(Profile::Permissive {
            creator: Some(creator),
        }) => {
            if let ObjectOwner::Principal { id, entity } = owner
                && id == LOCAL_CREATOR_ID
            {
                *entity = Some(creator.clone());
            }
        }
        Some(Profile::Permissive { creator: None } | Profile::TrustedLocal) | None => {}
    }
}

pub(super) fn reconcile(
    connection: &mut Connection,
    profile: &Profile,
) -> Result<(), CatalogError> {
    if !matches!(
        profile,
        Profile::Strict(_) | Profile::Permissive { creator: Some(_) }
    ) {
        return Ok(());
    }
    let mut after: Option<OwnedKey> = None;
    loop {
        let transaction = sql::immediate(connection)?;
        let read_owned = |row: &Row<'_>| {
            let key = OwnedKey {
                bucket: row.get(0)?,
                name: row.get(1)?,
                generation: row.get(2)?,
            };
            Ok((key, read(row, 3)?))
        };
        let row = match &after {
            None => sql::row(
                &transaction,
                "SELECT bucket,name,generation,owner FROM objects WHERE owner IS NOT NULL ORDER BY bucket,name,generation LIMIT 1",
                [],
                read_owned,
            ),
            Some(key) => sql::row(
                &transaction,
                "SELECT bucket,name,generation,owner FROM objects WHERE owner IS NOT NULL AND (bucket,name,generation)>(?1,?2,?3) ORDER BY bucket,name,generation LIMIT 1",
                params![key.bucket, key.name, key.generation],
                read_owned,
            ),
        }
        .optional()?;
        let Some((key, Some(mut owner))) = row else {
            return Ok(());
        };
        let before = owner.clone();
        canonicalize(&mut owner, Some(profile));
        if owner != before {
            reconcile_grant(
                &transaction,
                (&key.bucket, &key.name, key.generation),
                &before,
                &owner,
            )?;
            let owner = serde_json::to_string(&owner).map_err(unavailable)?;
            sql::exec(
                &transaction,
                "UPDATE objects SET owner=?4 WHERE bucket=?1 AND name=?2 AND generation=?3",
                params![key.bucket, key.name, key.generation, owner],
            )?;
        }
        transaction.commit()?;
        after = Some(key);
    }
}

/// The object version the owner walk resumes after.
struct OwnedKey {
    bucket: String,
    name: String,
    generation: i64,
}

fn reconcile_grant(
    connection: &Connection,
    key: (&str, &str, i64),
    before: &ObjectOwner,
    after: &ObjectOwner,
) -> Result<(), CatalogError> {
    let Some(previous) = before.entity() else {
        return Ok(());
    };
    let stored: Option<String> = sql::optional_scalar(
        connection,
        "SELECT entries FROM acl_object_entries WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
        params![key.0, key.1, key.2],
    )?;
    let Some(stored) = stored else { return Ok(()) };
    let mut entries = super::acl::decode(&stored, crate::acl::AclKind::Object)?;
    let Some(index) = entries
        .iter()
        .position(|entry| entry.entity == previous.as_str())
    else {
        return Ok(());
    };
    // An implicit grant follows the stable principal identity without leaving its old alias access.
    entries.remove(index);
    if let Some(entity) = after.entity() {
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.entity == entity.as_str())
        {
            existing.role = crate::acl::AclRole::Owner;
        } else {
            entries.insert(
                index,
                crate::acl::AclEntry {
                    entity: entity.into_string(),
                    role: crate::acl::AclRole::Owner,
                },
            );
        }
    }
    let stored = serde_json::to_string(&entries).map_err(unavailable)?;
    sql::exec(
        connection,
        "UPDATE acl_object_entries SET entries = ?4 WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
        params![key.0, key.1, key.2, stored],
    )?;
    Ok(())
}

pub(super) fn for_publication(
    connection: &Connection,
    bucket: &str,
    access: &BucketAccess,
    profile: Option<&Profile>,
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
    // The permissive profile has no caller, so the configured local creator stands in. Whether a
    // real caller would also own an object written through the administrative seeding path is not
    // distinguishable from the recordings; that path keeps the project owners.
    if matches!(access, BucketAccess::Permissive)
        && let Some(Profile::Permissive {
            creator: Some(creator),
        }) = profile
    {
        return Ok(ObjectOwner::Principal {
            id: LOCAL_CREATOR_ID.into(),
            entity: Some(creator.clone()),
        });
    }
    let project = sql::scalar(
        connection,
        "SELECT project FROM buckets WHERE name=?1",
        [bucket],
    )?;
    Ok(ObjectOwner::ProjectOwners { project })
}

pub(super) fn read(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<ObjectOwner>> {
    Ok(row
        .get::<_, Option<sql::Json<ObjectOwner>>>(index)?
        .map(|owner| owner.0))
}

pub(super) fn selected(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<Option<ObjectOwner>, CatalogError> {
    sql::row(
        connection,
        "SELECT owner FROM objects WHERE bucket=?1 AND name=?2 AND generation=?3",
        rusqlite::params![bucket, name, generation],
        |row| read(row, 0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(unavailable)
}
