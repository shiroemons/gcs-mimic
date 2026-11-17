//! Schema version 55. Earlier releases rendered the number of every project ID as 0, so ACLs
//! stored a bucket's own project teams as `project-owners-0` and the like. A project's number is
//! now derived from its ID ([`ProjectNumber`]), and this migration renames those stored entities
//! to the number of the project that owns the bucket.

use crate::{
    acl::{AclEntry, Entity, EntityRef},
    bucket::CatalogError,
    project::ProjectNumber,
    storage::catalog::unavailable,
};
use rusqlite::{Connection, params_from_iter, types::Value};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub(in crate::storage::catalog) const SCHEMA_VERSION: i64 = 55;

/// A stored column holding project-team entities, read with the key of its row.
struct AclColumn {
    /// Yields the `key_len` key columns, the stored JSON and the owning bucket's project.
    select: &'static str,
    /// Writes `?1` back to the row the remaining parameters key.
    update: &'static str,
    key_len: usize,
    shape: Shape,
}

#[derive(Clone, Copy)]
enum Shape {
    /// A JSON array of entries.
    Entries,
    /// A soft-deleted bucket's ACLs: a JSON array of `{scope, entries}`.
    Scoped,
}

/// A soft-deleted bucket keeps both of its ACLs, each with the scope it is restored to.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopedEntries {
    scope: String,
    entries: Vec<AclEntry>,
}

/// The retained objects of a soft-deleted bucket are keyed by `name#generation`, which no live
/// bucket name can spell, so an object row joins at most one of the two bucket tables.
const COLUMNS: [AclColumn; 3] = [
    AclColumn {
        select: "SELECT a.bucket, a.scope, a.entries, b.project FROM acl_bucket_entries a JOIN buckets b ON b.name = a.bucket",
        update: "UPDATE acl_bucket_entries SET entries = ?1 WHERE bucket = ?2 AND scope = ?3",
        key_len: 2,
        shape: Shape::Entries,
    },
    AclColumn {
        select: "SELECT e.bucket, e.name, e.generation, e.entries, coalesce(b.project, s.project) FROM acl_object_entries e LEFT JOIN buckets b ON b.name = e.bucket LEFT JOIN soft_deleted_buckets s ON s.name || '#' || s.generation = e.bucket WHERE coalesce(b.project, s.project) IS NOT NULL",
        update: "UPDATE acl_object_entries SET entries = ?1 WHERE bucket = ?2 AND name = ?3 AND generation = ?4",
        key_len: 3,
        shape: Shape::Entries,
    },
    AclColumn {
        select: "SELECT name, generation, acl, project FROM soft_deleted_buckets",
        update: "UPDATE soft_deleted_buckets SET acl = ?1 WHERE name = ?2 AND generation = ?3",
        key_len: 2,
        shape: Shape::Scoped,
    },
];

/// Renames every stored project-team entity numbered 0 to the owning project's number, in one
/// transaction with the version bump.
#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
pub(in crate::storage::catalog) fn migrate(
    connection: &mut Connection,
) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    for column in &COLUMNS {
        let mut select = transaction.prepare(column.select)?;
        let mut update = transaction.prepare(column.update)?;
        let mut rows = select.query([])?;
        // An update rewrites a non-key column, so the row keeps its place under the open scan.
        while let Some(row) = rows.next()? {
            let key = (0..column.key_len)
                .map(|index| row.get::<_, Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let json: String = row.get(column.key_len)?;
            let project: String = row.get(column.key_len + 1)?;
            if let Some(json) = column.shape.renumbered(&json, &project)? {
                update.execute(params_from_iter(
                    std::iter::once(Value::Text(json)).chain(key),
                ))?;
            }
        }
    }
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit().map_err(unavailable)
}

impl Shape {
    /// The rewritten JSON, or `None` when the column keeps its text.
    fn renumbered(self, json: &str, project: &str) -> Result<Option<String>, CatalogError> {
        match self {
            Self::Entries => rewrite(json, |entries: &mut Vec<AclEntry>| {
                renumber(entries, project)
            }),
            Self::Scoped => rewrite(json, |scopes: &mut Vec<ScopedEntries>| {
                let mut renamed = false;
                for scope in scopes {
                    renamed |= renumber(&mut scope.entries, project);
                }
                renamed
            }),
        }
    }
}

/// Decodes `json`, applies `rename` and encodes the result when it changed anything. A value
/// that does not decode is left for its reader, which reports the corruption with the resource
/// it belongs to instead of failing every open of the catalog.
fn rewrite<T: Serialize + DeserializeOwned>(
    json: &str,
    rename: impl FnOnce(&mut T) -> bool,
) -> Result<Option<String>, CatalogError> {
    let Ok(mut value) = serde_json::from_str::<T>(json) else {
        return Ok(None);
    };
    if !rename(&mut value) {
        return Ok(None);
    }
    serde_json::to_string(&value).map(Some).map_err(unavailable)
}

/// Renames the zero-numbered project teams in a bucket's or object's ACL to `project`'s own.
/// Returns whether any entry changed.
fn renumber(entries: &mut [AclEntry], project: &str) -> bool {
    // A numeric project ID always rendered as itself, so a team numbered 0 in its ACL names
    // project 0, not the owning project.
    if ProjectNumber::literal(project).is_some() {
        return false;
    }
    let mut renamed = false;
    for entry in entries {
        if let EntityRef::ProjectTeam { team, project: "0" } = entry.entity.parsed() {
            entry.entity = Entity::project_team(team, project);
            renamed = true;
        }
    }
    renamed
}

#[cfg(test)]
mod tests;
