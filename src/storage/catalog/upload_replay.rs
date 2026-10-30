use super::{Catalog, UploadGuard, objects, unavailable, unix_micros};
use crate::{
    bucket::{BucketAccess, CatalogError},
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::{Arc, OnceLock};

pub(crate) type UploadAclSnapshot = Arc<OnceLock<Option<Vec<crate::acl::AclEntry>>>>;

#[derive(serde::Serialize, serde::Deserialize)]
struct Completion {
    record: ObjectRecord,
    acl: Option<Vec<crate::acl::AclEntry>>,
}

const CAPACITY: i64 = 4096;
const LIFETIME_MICROS: i64 = 600_000_000;
pub(super) const SCHEMA: &str = "CREATE TABLE upload_replays (
    token BLOB PRIMARY KEY CHECK(length(token) = 32),
    request BLOB NOT NULL CHECK(length(request) = 32),
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    created INTEGER NOT NULL,
    deadline INTEGER NOT NULL CHECK(deadline = created + 600000000),
    fingerprint BLOB NOT NULL CHECK(length(fingerprint) = 32),
    completion TEXT NOT NULL CHECK(length(CAST(completion AS BLOB)) <= 65536)
) WITHOUT ROWID;
CREATE INDEX upload_replays_deadline ON upload_replays(deadline, token);
PRAGMA user_version = 35;";
const SELECT_INCARNATION: &str = "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1";

/// The client's idempotency token and the exact request it binds; without a token the
/// request is not replayable.
#[derive(Clone, Copy)]
pub(crate) struct ReplayInvocation<'a> {
    pub(crate) token: Option<&'a str>,
    pub(crate) request: &'a str,
}

/// The bucket a replayable copy reads from and the authority that reads it.
#[derive(Clone, Copy)]
pub(crate) struct CopyReplaySource<'a> {
    pub(crate) bucket: &'a str,
    pub(crate) access: &'a BucketAccess,
}

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    // The same statement as the per-upload `capacity` check, so validation reuses its entry.
    let count: i64 = connection
        .prepare_cached("SELECT count(*) FROM upload_replays")
        .and_then(|mut statement| statement.query_row([], |r| r.get(0)))
        .map_err(unavailable)?;
    if count > CAPACITY {
        return Err(unavailable("upload replay capacity exceeded"));
    }
    let mut statement = connection
        .prepare_cached("SELECT completion FROM upload_replays ORDER BY token")
        .map_err(unavailable)?;
    let mut rows = statement.query([]).map_err(unavailable)?;
    while let Some(row) = rows.next().map_err(unavailable)? {
        let completion: String = row.get(0).map_err(unavailable)?;
        let _: Completion = serde_json::from_str(&completion).map_err(unavailable)?;
    }
    Ok(())
}

fn hash(bytes: &[u8]) -> Vec<u8> {
    digest(&SHA256, bytes).as_ref().to_vec()
}

fn invalid() -> ObjectError {
    ObjectError::InvalidFixedMetadata("Idempotency token was reused for a different upload".into())
}

#[derive(Clone)]
pub(super) struct UploadReplay {
    token: Vec<u8>,
    request: Vec<u8>,
    incarnation: String,
    snapshot: UploadAclSnapshot,
}

#[derive(Clone)]
pub(crate) struct CopyReplay {
    replay: UploadReplay,
    fingerprint: Vec<u8>,
    source_bucket: String,
    source_incarnation: String,
    source_access: BucketAccess,
    destination_bucket: String,
    destination_access: BucketAccess,
}

impl CopyReplay {
    pub(crate) fn acl_snapshot(&self) -> UploadAclSnapshot {
        self.replay.snapshot.clone()
    }

    pub(super) fn cached(
        &self,
        connection: &Connection,
        now: i64,
    ) -> Result<Option<ObjectRecord>, ObjectError> {
        objects::authorized_bucket(
            connection,
            &self.destination_bucket,
            &self.destination_access,
        )?;
        objects::authorized_bucket(connection, &self.source_bucket, &self.source_access)?;
        self.replay
            .check_incarnation(connection, &self.destination_bucket)?;
        let incarnation: String = connection
            .prepare_cached(SELECT_INCARNATION)
            .and_then(|mut statement| statement.query_row([&self.source_bucket], |row| row.get(0)))
            .map_err(objects::object_unavailable)?;
        if incarnation != self.source_incarnation {
            return Err(ObjectError::PreconditionFailed);
        }
        self.replay.replay(connection, now, &self.fingerprint)
    }

    pub(super) fn store(
        &self,
        connection: &Connection,
        now: i64,
        record: &ObjectRecord,
    ) -> Result<(), ObjectError> {
        self.replay.store(
            connection,
            now,
            &self.fingerprint,
            record,
            &self.destination_access,
        )
    }
}

impl UploadReplay {
    fn cached(
        &self,
        connection: &Connection,
        now: i64,
    ) -> Result<Option<(Vec<u8>, Completion)>, ObjectError> {
        let row = connection.prepare_cached(
            "SELECT request, incarnation, fingerprint, completion FROM upload_replays WHERE token = ?1 AND deadline > ?2",
        ).and_then(|mut statement| statement.query_row(
            params![self.token, now],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?, r.get::<_, String>(3)?)),
        )).optional().map_err(objects::object_unavailable)?;
        row.map(|(request, incarnation, fingerprint, completion)| {
            if request != self.request || incarnation != self.incarnation {
                return Err(invalid());
            }
            Ok((
                fingerprint,
                serde_json::from_str(&completion).map_err(objects::object_unavailable)?,
            ))
        })
        .transpose()
    }

    pub(super) fn replay(
        &self,
        connection: &Connection,
        now: i64,
        fingerprint: &[u8],
    ) -> Result<Option<ObjectRecord>, ObjectError> {
        self.cached(connection, now)?
            .map(|(original, completion)| {
                if original != fingerprint {
                    return Err(invalid());
                }
                let _ = self.snapshot.set(completion.acl);
                Ok(completion.record)
            })
            .transpose()
    }

    pub(super) fn store(
        &self,
        connection: &Connection,
        now: i64,
        fingerprint: &[u8],
        record: &ObjectRecord,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let acl_access = match access {
            BucketAccess::Policy {
                identity, project, ..
            } => BucketAccess::Policy {
                identity: identity.clone(),
                project: project.clone(),
                permission: crate::auth::Permission::ObjectsGetIam,
            },
            other => other.clone(),
        };
        let visible = match objects::authorized_bucket(connection, &record.bucket, &acl_access) {
            Ok(_) => true,
            Err(ObjectError::Forbidden) => false,
            Err(error) => return Err(error),
        };
        let (uniform, project): (bool, String) = connection
            .prepare_cached("SELECT uniform_bucket_level_access_locked_time IS NOT NULL, project FROM buckets WHERE name = ?1")
            .and_then(|mut statement| statement.query_row([&record.bucket], |r| Ok((r.get(0)?, r.get(1)?))))
            .map_err(objects::object_unavailable)?;
        let acl = if visible && !uniform {
            let entries: Option<String> = connection
                .prepare_cached("SELECT entries FROM acl_object_entries WHERE bucket = ?1 AND name = ?2 AND generation = ?3")
                .and_then(|mut statement| statement.query_row(params![record.bucket, record.name, record.generation], |r| r.get(0)))
                .optional()
                .map_err(objects::object_unavailable)?;
            Some(
                entries
                    .map(|v| serde_json::from_str(&v).map_err(objects::object_unavailable))
                    .transpose()?
                    .unwrap_or_else(|| crate::acl::project_private(&project)),
            )
        } else {
            None
        };
        let completion = serde_json::to_string(&Completion {
            record: record.clone(),
            acl: acl.clone(),
        })
        .map_err(objects::object_unavailable)?;
        if completion.len() > 65536 {
            return Err(ObjectError::InvalidMetadata);
        }
        let deadline = now
            .checked_add(LIFETIME_MICROS)
            .ok_or_else(|| objects::object_unavailable("upload replay deadline overflow"))?;
        connection
            .prepare_cached("INSERT INTO upload_replays VALUES (?1,?2,?3,?4,?5,?6,?7)")
            .and_then(|mut statement| {
                statement.execute(params![
                    self.token,
                    self.request,
                    self.incarnation,
                    now,
                    deadline,
                    fingerprint,
                    completion
                ])
            })
            .map_err(objects::object_unavailable)?;
        let _ = self.snapshot.set(acl);
        Ok(())
    }

    pub(super) fn check_incarnation(
        &self,
        connection: &Connection,
        bucket: &str,
    ) -> Result<(), ObjectError> {
        let incarnation: String = connection
            .prepare_cached(SELECT_INCARNATION)
            .and_then(|mut statement| statement.query_row([bucket], |r| r.get(0)))
            .map_err(objects::object_unavailable)?;
        if incarnation != self.incarnation {
            return Err(ObjectError::PreconditionFailed);
        }
        Ok(())
    }
}

pub(super) fn capacity(connection: &Connection, now: i64) -> Result<(), ObjectError> {
    connection
        .prepare_cached("DELETE FROM upload_replays WHERE deadline <= ?1")
        .and_then(|mut statement| statement.execute([now]))
        .map_err(objects::object_unavailable)?;
    let count: i64 = connection
        .prepare_cached("SELECT count(*) FROM upload_replays")
        .and_then(|mut statement| statement.query_row([], |r| r.get(0)))
        .map_err(objects::object_unavailable)?;
    if count >= CAPACITY {
        return Err(objects::object_unavailable(
            "Upload replay capacity reached; retry after retained requests expire",
        ));
    }
    Ok(())
}

pub(super) fn fingerprint(value: &impl std::fmt::Debug) -> Vec<u8> {
    hash(format!("upload-v1:{value:?}").as_bytes())
}

impl Catalog {
    /// Copy replay binds both resource authorities and the exact invocation.
    pub(crate) async fn prepare_copy_replay(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
        source: CopyReplaySource<'_>,
        invocation: ReplayInvocation<'_>,
    ) -> Result<Option<CopyReplay>, ObjectError> {
        if invocation.token.is_none() {
            return Ok(None);
        }
        let source_bucket = source.bucket.to_owned();
        let source_access = source.access.clone();
        let source_query = source_bucket.clone();
        let source_authority = source_access.clone();
        let source_incarnation: String = self
            .run_waiting(move |connection| {
                Ok((|| {
                    objects::authorized_bucket(connection, &source_query, &source_authority)?;
                    connection
                        .prepare_cached(SELECT_INCARNATION)
                        .and_then(|mut statement| {
                            statement.query_row([&source_query], |row| row.get(0))
                        })
                        .map_err(objects::object_unavailable)
                })())
            })
            .await
            .map_err(objects::object_unavailable)??;
        let request = serde_json::to_string(&(invocation.request, &source_incarnation))
            .map_err(objects::object_unavailable)?;
        let mut permit = self.admit_object_upload()?;
        self.check_upload_replay(
            bucket,
            name,
            conditions,
            access,
            ReplayInvocation {
                token: invocation.token,
                request: &request,
            },
            &mut permit,
        )
        .await?;
        let replay = permit
            .replay
            .take()
            .ok_or_else(|| objects::object_unavailable("Missing copy replay binding"))?;
        Ok(Some(CopyReplay {
            replay,
            fingerprint: fingerprint(&request),
            source_bucket,
            source_incarnation,
            source_access,
            destination_bucket: bucket.to_owned(),
            destination_access: access.clone(),
        }))
    }

    pub(crate) async fn copy_replay_record(
        &self,
        replay: &CopyReplay,
    ) -> Result<Option<ObjectRecord>, ObjectError> {
        let replay = replay.clone();
        let clock = self.0.clock.clone();
        self.run_waiting(move |connection| {
            Ok(unix_micros(clock.now())
                .map_err(objects::object_unavailable)
                .and_then(|now| replay.cached(connection, now)))
        })
        .await
        .map_err(objects::object_unavailable)?
    }

    /// Upload advisory checks also bind the invocation and its admission permit.
    pub(crate) async fn check_upload_replay(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
        invocation: ReplayInvocation<'_>,
        permit: &mut UploadGuard,
    ) -> Result<(), ObjectError> {
        let ReplayInvocation { token, request } = invocation;
        let Some(token) = token else {
            return self.check_upload(bucket, name, conditions, access).await;
        };
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = access.clone();
        let token = hash(token.as_bytes());
        let expected_incarnation = permit.replay.as_ref().map(|r| r.incarnation.clone());
        let (profile, principal) = match &access {
            BucketAccess::Permissive => ("permissive", None),
            BucketAccess::Policy { identity: None, .. } => ("strict-anonymous", None),
            BucketAccess::Policy {
                identity: Some(identity),
                ..
            } => ("strict-principal", Some(identity.id())),
            BucketAccess::Admin(_) => return Err(ObjectError::Forbidden),
        };
        let request = hash(
            serde_json::to_vec(&(request, profile, principal))
                .map_err(objects::object_unavailable)?
                .as_slice(),
        );
        let clock = self.0.clock.clone();
        let replay = self
            .run_waiting(move |connection| {
                Ok((|| {
                    let transaction = connection
                        .transaction()
                        .map_err(objects::object_unavailable)?;
                    let (_, delete_permission) =
                        objects::authorized_bucket(&transaction, &bucket, &access)?;
                    let incarnation = transaction
                        .prepare_cached(SELECT_INCARNATION)
                        .and_then(|mut statement| statement.query_row([&bucket], |r| r.get(0)))
                        .map_err(objects::object_unavailable)?;
                    if expected_incarnation
                        .as_ref()
                        .is_some_and(|expected| expected != &incarnation)
                    {
                        return Err(ObjectError::PreconditionFailed);
                    }
                    let replay = UploadReplay {
                        token,
                        request,
                        incarnation,
                        snapshot: Arc::new(OnceLock::new()),
                    };
                    let now = unix_micros(clock.now()).map_err(objects::object_unavailable)?;
                    if replay.cached(&transaction, now)?.is_none() {
                        capacity(&transaction, now)?;
                        let previous = objects::lookup(&transaction, &bucket, &name)?;
                        objects::check_publication(
                            previous.as_ref(),
                            conditions,
                            delete_permission,
                            clock.now(),
                        )?;
                    }
                    Ok::<_, ObjectError>(replay)
                })())
            })
            .await
            .map_err(objects::object_unavailable)??;
        permit.replay = Some(replay);
        Ok(())
    }
}

impl UploadGuard {
    pub(crate) fn replay_acl_snapshot(&self) -> Option<UploadAclSnapshot> {
        self.replay.as_ref().map(|r| r.snapshot.clone())
    }
}
