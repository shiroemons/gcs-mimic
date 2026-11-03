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
use rusqlite::{Connection, OptionalExtension, Row, params, types::Type};

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
                *entity = Some(creator.to_string());
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
    let mut after: Option<(String, String, i64)> = None;
    loop {
        let transaction = sql::immediate(connection)?;
        let query = if after.is_some() {
            "SELECT bucket,name,generation,owner FROM objects WHERE owner IS NOT NULL AND (bucket,name,generation)>(?1,?2,?3) ORDER BY bucket,name,generation LIMIT 1"
        } else {
            "SELECT bucket,name,generation,owner FROM objects WHERE owner IS NOT NULL AND ?1 IS NULL AND ?2 IS NULL AND ?3 IS NULL ORDER BY bucket,name,generation LIMIT 1"
        };
        let row = sql::row(
            &transaction,
            query,
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
        .optional()?;
        let Some((bucket, name, generation, Some(mut owner))) = row else {
            return Ok(());
        };
        let before = owner.clone();
        canonicalize(&mut owner, Some(profile));
        if owner != before {
            reconcile_grant(&transaction, (&bucket, &name, generation), &before, &owner)?;
            let owner = serde_json::to_string(&owner).map_err(unavailable)?;
            sql::exec(
                &transaction,
                "UPDATE objects SET owner=?4 WHERE bucket=?1 AND name=?2 AND generation=?3",
                params![bucket, name, generation, owner],
            )?;
        }
        transaction.commit()?;
        after = Some((bucket, name, generation));
    }
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
    let mut entries: Vec<crate::acl::AclEntry> =
        serde_json::from_str(&stored).map_err(unavailable)?;
    let Some(index) = entries.iter().position(|entry| entry.entity == previous) else {
        return Ok(());
    };
    // An implicit grant follows the stable principal identity without leaving its old alias access.
    entries.remove(index);
    if let Some(entity) = after.entity() {
        if let Some(existing) = entries.iter_mut().find(|entry| entry.entity == entity) {
            existing.role = "OWNER".into();
        } else {
            entries.insert(
                index,
                crate::acl::AclEntry {
                    entity,
                    role: "OWNER".into(),
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
            entity: Some(creator.to_string()),
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
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            serde_json::from_str(&value)
                .map_err(|error| sql::conversion_error(index, Type::Text, error))
        })
        .transpose()
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
