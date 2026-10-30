use super::{
    BucketAccess, Catalog, CatalogError, Identity, OBJECTS_DELETE, Policy,
    objects::DeletePermission, unavailable,
};
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

const SELECT_BUCKET_PROJECT: &str = "SELECT project FROM buckets WHERE name = ?1";

#[expect(
    clippy::disallowed_methods,
    reason = "validates catalog state once at open"
)]
pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row("SELECT count(*) = 1 AND min(singleton) = 1 AND min(last_revision) >= (SELECT coalesce(max(revision), 0) FROM bucket_policies) FROM policy_revision", [], |row| row.get(0)).map_err(unavailable)?;
    let complete: bool = connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM buckets LEFT JOIN bucket_policies ON name = bucket WHERE bucket IS NULL) AND NOT EXISTS(SELECT 1 FROM bucket_policies LEFT JOIN buckets ON bucket = name WHERE name IS NULL)", [], |row| row.get(0)).map_err(unavailable)?;
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
    connection
        .prepare_cached("UPDATE policy_revision SET last_revision = last_revision + 1 WHERE singleton = 1 AND last_revision < 9223372036854775807 RETURNING last_revision")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(unavailable)
}

pub(super) fn insert_policy(
    connection: &Connection,
    bucket: &str,
    policy: &Policy,
) -> Result<(), CatalogError> {
    let revision = next_revision(connection)?;
    connection
        .prepare_cached("INSERT INTO bucket_policies VALUES (?1, ?2, ?3)")
        .and_then(|mut statement| {
            statement.execute(params![
                bucket,
                policy.bindings_json().to_string(),
                revision
            ])
        })
        .map_err(unavailable)?;
    Ok(())
}

pub(super) fn read_policy(connection: &Connection, bucket: &str) -> Result<Policy, CatalogError> {
    let (bindings, revision): (String, i64) = connection
        .prepare_cached("SELECT bindings, revision FROM bucket_policies WHERE bucket = ?1")
        .and_then(|mut statement| {
            statement.query_row([bucket], |row| Ok((row.get(0)?, row.get(1)?)))
        })
        .map_err(unavailable)?;
    Policy::from_stored(&bindings, crate::json::policy_etag(revision))
        .map_err(|reason| CatalogError::Corrupt(reason.into()))
}

pub(super) fn check_access(
    connection: &Connection,
    bucket: &str,
    project: &str,
    access: &BucketAccess,
) -> Result<DeletePermission, CatalogError> {
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
            .is_some_and(|identity| identity.allows(permission.as_str(), project))
            && !policy.allows(identity.as_ref(), permission.as_str(), pap)
        {
            return Err(CatalogError::Forbidden);
        }
        let delete = identity
            .as_ref()
            .is_some_and(|identity| identity.allows(OBJECTS_DELETE, project))
            || policy.allows(identity.as_ref(), OBJECTS_DELETE, pap);
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
        if projects.len() > 64 {
            return Err(CatalogError::InvalidMetadata(
                "Parent policy exceeds 64 projects".into(),
            ));
        }
        let projects = projects.to_vec();
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            transaction
                .execute("DELETE FROM parent_public_access_prevention", [])
                .map_err(unavailable)?;
            for project in &projects {
                crate::json::validate_project(project).map_err(|_| {
                    CatalogError::InvalidMetadata("Invalid parent policy project".into())
                })?;
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO parent_public_access_prevention VALUES (?1)",
                        [project],
                    )
                    .map_err(unavailable)?;
            }
            super::change_journal::parent_pap(&transaction, &projects)?;
            transaction.commit().map_err(unavailable)
        })
        .await
    }

    pub(crate) async fn bucket_access<'a>(
        &self,
        bucket: &str,
        identity: impl Into<Option<&'a Identity>>,
        permission: &str,
    ) -> Result<BucketAccess, CatalogError> {
        let bucket = bucket.to_owned();
        let identity = identity.into().cloned();
        let permission = crate::auth::Permission::try_from(permission)
            .map_err(|error| CatalogError::InvalidMetadata(error.into()))?;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let project: Option<String> = transaction
                .prepare_cached(SELECT_BUCKET_PROJECT)
                .and_then(|mut statement| statement.query_row([&bucket], |row| row.get(0)))
                .optional()
                .map_err(unavailable)?;
            if project.is_none()
                && !identity
                    .as_ref()
                    .is_some_and(|identity| identity.allows_anywhere(permission.as_str()))
            {
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
            if public_access_prevention(&transaction, &bucket)?
                && policy.bindings.iter().any(|binding| {
                    binding.members.iter().any(|member| {
                        matches!(member.as_str(), "allUsers" | "allAuthenticatedUsers")
                    })
                })
            {
                return Err(CatalogError::PublicAccessPrevention);
            }
            let revision = next_revision(&transaction)?;
            transaction
                .prepare_cached(
                    "UPDATE bucket_policies SET bindings = ?2, revision = ?3 WHERE bucket = ?1",
                )
                .and_then(|mut statement| {
                    statement.execute(params![
                        bucket,
                        policy.bindings_json().to_string(),
                        revision
                    ])
                })
                .map_err(unavailable)?;
            super::change_journal::append(
                &transaction,
                super::change_journal::origin(&access),
                super::change_journal::ChangeKind::Iam,
                &bucket,
                None,
            )?;
            transaction.commit().map_err(unavailable)?;
            policy.etag = Some(crate::json::policy_etag(revision));
            Ok(policy)
        })
        .await
    }
}

pub(super) fn public_access_prevention(
    connection: &Connection,
    bucket: &str,
) -> Result<bool, CatalogError> {
    connection
        .prepare_cached("SELECT public_access_prevention_enforced OR EXISTS(SELECT 1 FROM parent_public_access_prevention WHERE project = buckets.project) FROM buckets WHERE name = ?1")
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
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
    let project: String = connection
        .prepare_cached(SELECT_BUCKET_PROJECT)
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
        .optional()
        .map_err(unavailable)?
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
                        .query_map([], |row| row.get(0))
                        .map_err(unavailable)?
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
