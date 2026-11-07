use super::{
    BucketAccess, Catalog, CatalogError, Identity, OBJECTS_DELETE, Policy,
    objects::DeletePermission, unavailable,
};
use crate::storage::catalog::sql;
use rusqlite::{Connection, OptionalExtension, params};

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

/// A policy's documented 1,500 principals can take more than the 64 KiB this column first allowed.
/// The new check backs up [`encoded_bindings`], which refuses a larger policy as a client error
/// before this could fail as a retryable server error. No table references this one, so dropping
/// it cascades nothing.
pub(super) const RAISED_BINDINGS_SCHEMA: &str = "CREATE TABLE bucket_policies_rebuilt (
    bucket TEXT PRIMARY KEY COLLATE BINARY,
    bindings TEXT NOT NULL CHECK(length(CAST(bindings AS BLOB)) <= 1048576),
    revision INTEGER NOT NULL CHECK(revision > 0)
) WITHOUT ROWID;
INSERT INTO bucket_policies_rebuilt SELECT bucket, bindings, revision FROM bucket_policies;
DROP TABLE bucket_policies;
ALTER TABLE bucket_policies_rebuilt RENAME TO bucket_policies;
PRAGMA user_version = 51;";

/// The API refuses more than the documented 1,500 principals, but an ACL edit adds members of its
/// own, up to one full ACL.
const MAX_STORED_MEMBERS: usize = 1500 + crate::acl::MAX_ENTRIES;
/// Matches the column's check. Every stored member fits in about 270 bytes, so real policies stay
/// under half of this; only unbounded text such as a custom role name can reach it.
const MAX_BINDINGS_BYTES: usize = 1024 * 1024;

const SELECT_BUCKET_PROJECT: &str = "SELECT project FROM buckets WHERE name = ?1";

/// Encodes a policy for its row. Every writer passes here, whichever API it serves, so nothing can
/// grow the column past its bounds or fail its check as a retryable server error.
fn encoded_bindings(policy: &Policy) -> Result<String, CatalogError> {
    let members: usize = policy
        .bindings
        .iter()
        .map(|binding| binding.members.len())
        .sum();
    if members > MAX_STORED_MEMBERS {
        return Err(CatalogError::InvalidMetadata(format!(
            "Policy exceeds {MAX_STORED_MEMBERS} principal occurrences, counting the ACL's grants; remove some before adding more."
        )));
    }
    let encoded = policy.bindings_json().to_string();
    if encoded.len() > MAX_BINDINGS_BYTES {
        return Err(CatalogError::InvalidMetadata(
            "Policy exceeds 1 MiB once encoded; shorten its role names.".into(),
        ));
    }
    Ok(encoded)
}

#[expect(
    clippy::disallowed_methods,
    reason = "validates catalog state once at open"
)]
pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row("SELECT count(*) = 1 AND min(singleton) = 1 AND min(last_revision) >= (SELECT coalesce(max(revision), 0) FROM bucket_policies) FROM policy_revision", [], |row| row.get(0))?;
    let complete: bool = connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM buckets LEFT JOIN bucket_policies ON name = bucket WHERE bucket IS NULL) AND NOT EXISTS(SELECT 1 FROM bucket_policies LEFT JOIN buckets ON bucket = name WHERE name IS NULL)", [], |row| row.get(0))?;
    if !valid || !complete {
        return Err(unavailable("invalid bucket policy catalog state"));
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration step"
)]
pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    transaction.execute("INSERT INTO policy_revision VALUES (1, 0)", [])?;
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
            .optional()?;
        let Some((name, project)) = bucket else { break };
        insert_policy(&transaction, &name, &Policy::default_for(&project))?;
        after = Some(name);
    }
    transaction.commit().map_err(unavailable)
}

pub(super) fn next_revision(connection: &Connection) -> Result<i64, CatalogError> {
    sql::scalar(connection, "UPDATE policy_revision SET last_revision = last_revision + 1 WHERE singleton = 1 AND last_revision < 9223372036854775807 RETURNING last_revision", [])
        .map_err(unavailable)
}

pub(super) fn insert_policy(
    connection: &Connection,
    bucket: &str,
    policy: &Policy,
) -> Result<(), CatalogError> {
    let encoded = encoded_bindings(policy)?;
    let revision = next_revision(connection)?;
    sql::exec(
        connection,
        "INSERT INTO bucket_policies VALUES (?1, ?2, ?3)",
        params![bucket, encoded, revision],
    )?;
    Ok(())
}

pub(super) fn read_policy(connection: &Connection, bucket: &str) -> Result<Policy, CatalogError> {
    let (bindings, revision): (String, i64) = sql::row(
        connection,
        "SELECT bindings, revision FROM bucket_policies WHERE bucket = ?1",
        [bucket],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Policy::from_stored(&bindings, crate::json::policy_etag(revision))
        .map_err(|reason| CatalogError::Corrupt(reason.into()))
}

pub(super) fn store_policy(
    connection: &Connection,
    bucket: &str,
    policy: &mut Policy,
) -> Result<(), CatalogError> {
    let encoded = encoded_bindings(policy)?;
    let revision = next_revision(connection)?;
    sql::exec(
        connection,
        "UPDATE bucket_policies SET bindings = ?2, revision = ?3 WHERE bucket = ?1",
        params![bucket, encoded, revision],
    )?;
    policy.etag = Some(crate::json::policy_etag(revision));
    Ok(())
}

pub(super) fn advance_acl_revision(
    connection: &Connection,
    bucket: &str,
    updated: i64,
) -> Result<(), CatalogError> {
    let changed = sql::exec(
        connection,
        "UPDATE buckets SET metageneration=metageneration+1,updated=?2,updated_nanos=0 WHERE name=?1 AND metageneration<9223372036854775807",
        params![bucket, updated],
    )?;
    if changed != 1 {
        return Err(unavailable("bucket metageneration exhausted"));
    }
    Ok(())
}

pub(super) fn check_access(
    connection: &Connection,
    bucket: &str,
    project: &str,
    access: &BucketAccess,
) -> Result<DeletePermission, CatalogError> {
    if let BucketAccess::Admin(grant) = access {
        grant.authorize(connection)?;
    }
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
        let pap = policy_public_access(connection, bucket)?;
        if !identity
            .as_ref()
            .is_some_and(|identity| identity.allows(*permission, project))
            && !policy.allows(identity.as_ref(), *permission, pap)
            && !super::acl::bucket_granted(
                connection,
                bucket,
                project,
                identity.as_ref(),
                *permission,
            )?
        {
            return Err(CatalogError::Forbidden);
        }
        let delete = identity
            .as_ref()
            .is_some_and(|identity| identity.allows(OBJECTS_DELETE, project))
            || policy.allows(identity.as_ref(), OBJECTS_DELETE, pap)
            || super::acl::bucket_granted(
                connection,
                bucket,
                project,
                identity.as_ref(),
                OBJECTS_DELETE,
            )?;
        return Ok(if delete {
            DeletePermission::Granted
        } else {
            DeletePermission::Denied
        });
    }
    Ok(DeletePermission::Granted)
}

impl Catalog {
    /// Install a startup-only local simulation of effective parent policy; never persisted as bucket metadata.
    #[expect(
        clippy::disallowed_methods,
        reason = "parent policy is installed once at startup"
    )]
    pub async fn configure_public_access_prevention(
        &self,
        projects: &[String],
    ) -> Result<(), CatalogError> {
        let projects = projects.to_vec();
        let parent_policy = self.0.parent_policy.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            transaction.execute("DELETE FROM parent_public_access_prevention", [])?;
            for project in &projects {
                crate::json::validate_project(project).map_err(|_| {
                    CatalogError::InvalidMetadata("Invalid parent policy project".into())
                })?;
                transaction.execute(
                    "INSERT OR IGNORE INTO parent_public_access_prevention VALUES (?1)",
                    [project],
                )?;
            }
            super::change_journal::parent_pap(&transaction, &projects)?;
            transaction.commit().map_err(unavailable)?;
            parent_policy.publish(&projects);
            Ok(())
        })
        .await
    }

    pub(crate) async fn bucket_access<'a>(
        &self,
        bucket: &str,
        identity: impl Into<Option<&'a Identity>>,
        permission: crate::auth::Permission,
    ) -> Result<BucketAccess, CatalogError> {
        let bucket = bucket.to_owned();
        let identity = identity.into().cloned();
        self.run_read(move |connection| {
            let snapshot = sql::read_snapshot(connection)?;
            let project: Option<String> =
                sql::optional_scalar(&snapshot, SELECT_BUCKET_PROJECT, [&bucket])?;
            if project.is_none()
                && !identity
                    .as_ref()
                    .is_some_and(|identity| identity.allows_anywhere(permission))
            {
                return Err(CatalogError::Forbidden);
            }
            let access = BucketAccess::Policy {
                identity: identity.clone(),
                permission,
                project: project.clone(),
            };
            // Object ACLs can only be evaluated after selecting the exact object generation.
            if let Some(project) = project
                && !crate::acl::object_permission(permission)
            {
                check_access(&snapshot, &bucket, &project, &access)?;
            }
            Ok(access)
        })
        .await
    }

    pub(crate) async fn has_bucket_permission(
        &self,
        bucket: &str,
        access: &BucketAccess,
    ) -> Result<bool, CatalogError> {
        let bucket = bucket.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let project: String =
                sql::optional_scalar(&transaction, SELECT_BUCKET_PROJECT, [&bucket])?
                    .ok_or(CatalogError::NotFound)?;
            match check_access(&transaction, &bucket, &project, &access) {
                Ok(_) => Ok(true),
                Err(CatalogError::Forbidden) => Ok(false),
                Err(error) => Err(error),
            }
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
            let transaction = connection.transaction()?;
            authorize_policy(&transaction, &bucket, &access)?;
            read_policy(&transaction, &bucket)
        })
        .await
    }

    pub(crate) async fn set_policy(
        &self,
        bucket: &str,
        policy: Policy,
        access: &BucketAccess,
    ) -> Result<Policy, CatalogError> {
        self.set_policy_checked(bucket, policy, access, |_| Ok(()))
            .await
    }

    /// Replaces the policy once `check` accepts the current policy inside the write
    /// transaction, so an entity-tag precondition cannot pass a concurrently replaced policy.
    pub(crate) async fn set_policy_checked(
        &self,
        bucket: &str,
        mut policy: Policy,
        access: &BucketAccess,
        check: impl FnOnce(&Policy) -> Result<(), CatalogError> + Send + 'static,
    ) -> Result<Policy, CatalogError> {
        let bucket = bucket.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        let principals = self.0.principals.get().cloned();
        self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            authorize_policy(&transaction, &bucket, &access)?;
            let current = read_policy(&transaction, &bucket)?;
            check(&current)?;
            if policy.etag.is_some() && policy.etag != current.etag {
                return Err(CatalogError::PreconditionFailed);
            }
            if public_access_prevention(&transaction, &bucket)?
                && policy.bindings.iter().any(|binding| {
                    binding.members.iter().any(|member| {
                        matches!(member.as_str(), "allUsers" | "allAuthenticatedUsers")
                    })
                })
            {
                return Err(CatalogError::PublicAccessPrevention);
            }
            super::acl::replace_from_policy(&transaction, &bucket, &policy, principals.as_ref())?;
            // Every accepted policy write advanced the bucket metageneration on Cloud Storage,
            // including a replay of the current policy (recorded 2026-10-31).
            let updated = super::unix_micros(clock.now())?;
            advance_acl_revision(&transaction, &bucket, updated)?;
            store_policy(&transaction, &bucket, &mut policy)?;
            super::change_journal::append(
                &transaction,
                super::change_journal::origin(&access),
                super::change_journal::ChangeKind::Iam,
                &bucket,
                None,
            )?;
            transaction.commit()?;
            Ok(policy)
        })
        .await
    }
}

pub(super) fn public_access_prevention(
    connection: &Connection,
    bucket: &str,
) -> Result<bool, CatalogError> {
    sql::scalar(connection, "SELECT public_access_prevention_enforced OR EXISTS(SELECT 1 FROM parent_public_access_prevention WHERE project = buckets.project) FROM buckets WHERE name = ?1", [bucket])
        .map_err(unavailable)
}

/// Whether public members of the bucket's IAM policy may grant access.
pub(super) fn policy_public_access(
    connection: &Connection,
    bucket: &str,
) -> Result<crate::auth::PublicAccess, CatalogError> {
    use crate::auth::PublicAccess;
    Ok(if public_access_prevention(connection, bucket)? {
        PublicAccess::Prevented
    } else {
        PublicAccess::Allowed
    })
}

fn authorize_policy(
    connection: &Connection,
    bucket: &str,
    access: &BucketAccess,
) -> Result<(), CatalogError> {
    let project: String = sql::optional_scalar(connection, SELECT_BUCKET_PROJECT, [bucket])?
        .ok_or(CatalogError::NotFound)?;
    check_access(connection, bucket, &project, access)?;
    Ok(())
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::{
        future::Future,
        sync::mpsc,
        task::Poll,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn cancelled_policy_request_still_executes_its_owned_job() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let path = std::env::temp_dir().join(format!(
                "gcs-mimic-policy-job-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let catalog = Catalog::open(&path).await.unwrap();
            catalog
                .configure_public_access_prevention(&["old-project".into()])
                .await
                .unwrap();
            let (entered, blocked) = tokio::sync::oneshot::channel();
            let (release, released) = mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            });
            blocked.await.unwrap();
            {
                let projects = ["new-project".into()];
                let work = catalog.configure_public_access_prevention(&projects);
                tokio::pin!(work);
                std::future::poll_fn(|cx| {
                    assert!(work.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
            }
            release.send(()).unwrap();
            blocker.await.unwrap();
            let projects: Vec<String> = catalog
                .run(|connection| {
                    let mut statement = connection
                        .prepare(
                            "SELECT project FROM parent_public_access_prevention ORDER BY project",
                        )
                        .map_err(unavailable)?;
                    statement
                        .query_map([], |row| row.get(0))?
                        .collect::<Result<_, _>>()
                        .map_err(unavailable)
                })
                .await
                .unwrap();
            assert_eq!(
                projects,
                ["new-project"],
                "cancelled request must not abort its queued SQLite mutation"
            );
            catalog.close().await.unwrap();
            std::fs::remove_dir_all(path).unwrap();
        });
    }
}
