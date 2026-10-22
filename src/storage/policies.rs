use super::{BucketAccess, Catalog, CatalogError, Identity, OBJECTS_DELETE, Policy, unavailable};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(super) const SCHEMA: &str = "CREATE TABLE bucket_policies (
    bucket TEXT PRIMARY KEY COLLATE BINARY,
    bindings TEXT NOT NULL CHECK(length(CAST(bindings AS BLOB)) <= 65536),
    revision INTEGER NOT NULL CHECK(revision > 0)
) WITHOUT ROWID;
CREATE TABLE policy_revision (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    last_revision INTEGER NOT NULL CHECK(last_revision >= 0)
) WITHOUT ROWID;
PRAGMA user_version = 6;";

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row("SELECT count(*) = 1 AND min(singleton) = 1 AND min(last_revision) >= (SELECT coalesce(max(revision), 0) FROM bucket_policies) FROM policy_revision", [], |row| row.get(0)).map_err(unavailable)?;
    let complete: bool = connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM buckets LEFT JOIN bucket_policies ON name = bucket WHERE bucket IS NULL) AND NOT EXISTS(SELECT 1 FROM bucket_policies LEFT JOIN buckets ON bucket = name WHERE name IS NULL)", [], |row| row.get(0)).map_err(unavailable)?;
    if !valid || !complete {
        return Err(unavailable("invalid bucket policy catalog state"));
    }
    Ok(())
}

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction
        .execute("INSERT INTO policy_revision VALUES (1, 0)", [])
        .map_err(unavailable)?;
    // Keyset iteration keeps migration memory independent of the number of buckets.
    let mut after: Option<String> = None;
    loop {
        let sql = if after.is_some() {
            "SELECT name, project FROM buckets WHERE name > ?1 ORDER BY name LIMIT 1"
        } else {
            "SELECT name, project FROM buckets WHERE ?1 IS NULL ORDER BY name LIMIT 1"
        };
        let bucket: Option<(String, String)> = transaction
            .query_row(sql, [&after], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(unavailable)?;
        let Some((name, project)) = bucket else { break };
        insert_policy(&transaction, &name, &Policy::default_for(&project))?;
        after = Some(name);
    }
    transaction.commit().map_err(unavailable)
}

fn next_revision(connection: &Connection) -> Result<i64, CatalogError> {
    connection.query_row("UPDATE policy_revision SET last_revision = last_revision + 1 WHERE singleton = 1 AND last_revision < 9223372036854775807 RETURNING last_revision", [], |row| row.get(0)).map_err(unavailable)
}

pub(super) fn insert_policy(
    connection: &Connection,
    bucket: &str,
    policy: &Policy,
) -> Result<(), CatalogError> {
    let revision = next_revision(connection)?;
    connection
        .execute(
            "INSERT INTO bucket_policies VALUES (?1, ?2, ?3)",
            params![bucket, policy.bindings_json().to_string(), revision],
        )
        .map_err(unavailable)?;
    Ok(())
}

fn read_policy(connection: &Connection, bucket: &str) -> Result<Policy, CatalogError> {
    let (bindings, revision): (String, i64) = connection
        .query_row(
            "SELECT bindings, revision FROM bucket_policies WHERE bucket = ?1",
            [bucket],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(unavailable)?;
    Policy::from_stored(&bindings, crate::json::policy_etag(revision)).map_err(unavailable)
}

pub(super) fn check_access(
    connection: &Connection,
    bucket: &str,
    project: &str,
    access: &BucketAccess,
) -> Result<bool, CatalogError> {
    if let BucketAccess::Policy {
        identity,
        permission,
        project: observed,
    } = access
    {
        if observed.as_deref().is_some_and(|owner| owner != project) {
            return Err(CatalogError::Forbidden);
        }
        let policy = read_policy(connection, bucket)?;
        if !identity.allows(permission, project) && !policy.allows(identity, permission) {
            return Err(CatalogError::Forbidden);
        }
        return Ok(
            identity.allows(OBJECTS_DELETE, project) || policy.allows(identity, OBJECTS_DELETE)
        );
    }
    access.check(project).ok_or(CatalogError::Forbidden)
}

impl Catalog {
    pub(crate) async fn bucket_access(
        &self,
        bucket: &str,
        identity: &Identity,
        permission: &str,
    ) -> Result<BucketAccess, CatalogError> {
        let bucket = bucket.to_owned();
        let identity = identity.clone();
        let permission = permission.to_owned();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let project: Option<String> = transaction
                .query_row(
                    "SELECT project FROM buckets WHERE name = ?1",
                    [&bucket],
                    |row| row.get(0),
                )
                .optional()
                .map_err(unavailable)?;
            if project.is_none() && !identity.allows_anywhere(&permission) {
                return Err(CatalogError::Forbidden);
            }
            let access = BucketAccess::Policy {
                identity,
                permission,
                project: project.clone(),
            };
            if let Some(project) = project {
                check_access(&transaction, &bucket, &project, &access)?;
            }
            Ok(access)
        })
        .await
    }

    pub(crate) async fn get_policy(
        &self,
        bucket: &str,
        access: &BucketAccess,
    ) -> Result<Policy, CatalogError> {
        let bucket = bucket.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            authorize_policy(&transaction, &bucket, &access)?;
            read_policy(&transaction, &bucket)
        })
        .await
    }

    pub(crate) async fn set_policy(
        &self,
        bucket: &str,
        mut policy: Policy,
        access: &BucketAccess,
    ) -> Result<Policy, CatalogError> {
        let bucket = bucket.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            authorize_policy(&transaction, &bucket, &access)?;
            let current = read_policy(&transaction, &bucket)?;
            if policy.etag.is_some() && policy.etag != current.etag {
                return Err(CatalogError::PreconditionFailed);
            }
            let revision = next_revision(&transaction)?;
            transaction
                .execute(
                    "UPDATE bucket_policies SET bindings = ?2, revision = ?3 WHERE bucket = ?1",
                    params![bucket, policy.bindings_json().to_string(), revision],
                )
                .map_err(unavailable)?;
            transaction.commit().map_err(unavailable)?;
            policy.etag = Some(crate::json::policy_etag(revision));
            Ok(policy)
        })
        .await
    }
}

fn authorize_policy(
    connection: &Connection,
    bucket: &str,
    access: &BucketAccess,
) -> Result<(), CatalogError> {
    let project: String = connection
        .query_row(
            "SELECT project FROM buckets WHERE name = ?1",
            [bucket],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?
        .ok_or(CatalogError::NotFound)?;
    check_access(connection, bucket, &project, access)?;
    Ok(())
}
