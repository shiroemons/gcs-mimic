use super::{Catalog, UploadGuard, objects, unix_micros};
use crate::storage::catalog::{bucket_incarnation, sql};
use crate::{
    bucket::{BucketAccess, CatalogError},
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::{Arc, OnceLock};

mod fingerprint;
pub(super) use fingerprint::{UploadFingerprint, copy_fingerprint};

pub(crate) type UploadAclSnapshot = Arc<OnceLock<Option<super::acl::ObjectAclSnapshot>>>;

#[derive(serde::Serialize, serde::Deserialize)]
struct Completion {
    record: ObjectRecord,
    acl: Option<Vec<crate::acl::AclEntry>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<CopySource>,
}

fn decode_completion(stored: &str, index: usize) -> rusqlite::Result<Completion> {
    let conversion = |error| sql::conversion_error(index, rusqlite::types::Type::Text, error);
    let completion: Completion = serde_json::from_str(stored).map_err(conversion)?;
    for entry in completion.acl.iter().flatten() {
        crate::acl::validate_role(crate::acl::AclKind::Object, entry.role.as_str())
            .map_err(|error| sql::conversion_error(index, rusqlite::types::Type::Text, error))?;
    }
    Ok(completion)
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct CopySource {
    bucket: String,
    name: String,
    generation: i64,
}

/// How many completions a retry can get back. This is a window over recent uploads, not a quota:
/// a full table makes room by dropping its oldest record, see [`make_room`].
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
    pub(crate) name: &'a str,
    pub(crate) generation: Option<i64>,
    pub(crate) access: &'a BucketAccess,
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let mut statement =
        connection.prepare_cached("SELECT completion FROM upload_replays ORDER BY token")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let completion: String = row.get(0)?;
        decode_completion(&completion, 0)?;
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
    principals: Option<super::principals::Profile>,
}

#[derive(Clone)]
pub(crate) struct CopyReplay {
    replay: UploadReplay,
    fingerprint: Vec<u8>,
    source_bucket: String,
    source_name: String,
    source_generation: Option<i64>,
    source_incarnation: String,
    source_access: BucketAccess,
    destination_bucket: String,
    destination_access: BucketAccess,
}

impl CopyReplay {
    pub(crate) fn bind_source(&mut self, source: &ObjectRecord) {
        self.source_generation = Some(source.generation);
    }
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
        self.replay
            .check_incarnation(connection, &self.destination_bucket)?;
        let incarnation: String = bucket_incarnation(connection, &self.source_bucket)
            .optional()?
            .ok_or(ObjectError::PreconditionFailed)?;
        if incarnation != self.source_incarnation {
            return Err(ObjectError::PreconditionFailed);
        }
        let cached = self.replay.cached(connection, now)?;
        let Some((_, completion)) = cached else {
            return Ok(None);
        };
        match completion.source {
            Some(source)
                if source.bucket == self.source_bucket && source.name == self.source_name =>
            {
                objects::authorize_generation(
                    connection,
                    &source.bucket,
                    &source.name,
                    source.generation,
                    &self.source_access,
                )?;
            }
            _ => {
                objects::authorized_bucket(connection, &self.source_bucket, &self.source_access)?;
            }
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
            Some(CopySource {
                bucket: self.source_bucket.clone(),
                name: self.source_name.clone(),
                generation: self
                    .source_generation
                    .ok_or_else(|| objects::object_unavailable("Missing copy source generation"))?,
            }),
        )
    }
}

impl UploadReplay {
    fn cached(
        &self,
        connection: &Connection,
        now: i64,
    ) -> Result<Option<(Vec<u8>, Completion)>, ObjectError> {
        let row = sql::row(connection,
            "SELECT request, incarnation, fingerprint, completion FROM upload_replays WHERE token = ?1 AND deadline > ?2",
            params![self.token, now],
            |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?, r.get::<_, String>(3)?)),
        ).optional()?;
        row.map(|(request, incarnation, fingerprint, completion)| {
            if request != self.request || incarnation != self.incarnation {
                return Err(invalid());
            }
            Ok((fingerprint, decode_completion(&completion, 3)?))
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
            .map(|(original, mut completion)| {
                if original != fingerprint {
                    return Err(invalid());
                }
                if let Some(owner) = &mut completion.record.owner {
                    super::object_owner::canonicalize(owner, self.principals.as_ref());
                }
                let snapshot = completion.acl.map(|entries| {
                    super::acl::ObjectAclSnapshot::new(
                        &completion.record,
                        entries,
                        &self.incarnation,
                    )
                });
                let _ = self.snapshot.set(snapshot);
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
        source: Option<CopySource>,
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
        let visible = match objects::authorize_generation(
            connection,
            &record.bucket,
            &record.name,
            record.generation,
            &acl_access,
        ) {
            Ok(_) => true,
            Err(ObjectError::Forbidden) => false,
            Err(error) => return Err(error),
        };
        let (uniform, project): (bool, String) = sql::row(
            connection,
            "SELECT uniform_bucket_level_access_locked_time IS NOT NULL, project FROM buckets WHERE name = ?1",
            [&record.bucket],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let acl = if visible && !uniform {
            let entries: Option<String> = sql::optional_scalar(
                connection,
                "SELECT entries FROM acl_object_entries WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                params![record.bucket, record.name, record.generation],
            )?;
            Some(
                entries
                    .map(|v| {
                        super::acl::decode(&v, crate::acl::AclKind::Object)
                            .map_err(ObjectError::from)
                    })
                    .transpose()?
                    .unwrap_or_else(|| crate::acl::project_private(&project)),
            )
        } else {
            None
        };
        let completion = serde_json::to_string(&Completion {
            record: record.clone(),
            acl: acl.clone(),
            source,
        })
        .map_err(objects::object_unavailable)?;
        if completion.len() > 65536 {
            return Err(ObjectError::InvalidMetadata);
        }
        let deadline = now
            .checked_add(LIFETIME_MICROS)
            .ok_or_else(|| objects::object_unavailable("upload replay deadline overflow"))?;
        sql::exec(
            connection,
            "INSERT INTO upload_replays VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                self.token,
                self.request,
                self.incarnation,
                now,
                deadline,
                fingerprint,
                completion
            ],
        )?;
        let snapshot = acl
            .map(|entries| super::acl::ObjectAclSnapshot::new(record, entries, &self.incarnation));
        let _ = self.snapshot.set(snapshot);
        Ok(())
    }

    pub(super) fn check_incarnation(
        &self,
        connection: &Connection,
        bucket: &str,
    ) -> Result<(), ObjectError> {
        let incarnation: String = bucket_incarnation(connection, bucket)?;
        if incarnation != self.incarnation {
            return Err(ObjectError::PreconditionFailed);
        }
        Ok(())
    }
}

/// Makes room for one more completion, in the publishing transaction, so a rejected upload drops
/// nothing. Only finished uploads have a record (the bound request lives in the upload's guard
/// until its record commits), so this never touches an upload in progress. A retry that finds
/// its record gone publishes again, exactly as it does after the record's lifetime.
pub(super) fn make_room(connection: &Connection, now: i64) -> Result<(), ObjectError> {
    sql::exec(
        connection,
        "DELETE FROM upload_replays WHERE deadline <= ?1",
        [now],
    )?;
    sql::exec(
        connection,
        "DELETE FROM upload_replays WHERE token IN (SELECT token FROM upload_replays ORDER BY deadline, token LIMIT max((SELECT count(*) FROM upload_replays) + 1 - ?1, 0))",
        [CAPACITY],
    )?;
    Ok(())
}

fn copy_source_generation(
    connection: &Connection,
    source: CopyReplaySource<'_>,
    token: &[u8],
    now: i64,
) -> Result<Option<i64>, ObjectError> {
    if objects::object_acl_authority(connection, source.bucket, source.access)?.is_none() {
        return Ok(source.generation);
    }
    let stored: Option<String> = sql::optional_scalar(
        connection,
        "SELECT completion FROM upload_replays WHERE token=?1 AND deadline>?2",
        params![token, now],
    )?;
    let generation = match stored {
        Some(stored) => {
            let completion = decode_completion(&stored, 0)?;
            match completion.source {
                Some(original)
                    if original.bucket == source.bucket && original.name == source.name =>
                {
                    original.generation
                }
                _ => return Err(ObjectError::Forbidden),
            }
        }
        None => match source.generation {
            Some(generation) => generation,
            None => {
                objects::lookup(connection, source.bucket, source.name)?
                    .ok_or(ObjectError::Forbidden)?
                    .record
                    .generation
            }
        },
    };
    objects::authorize_generation(
        connection,
        source.bucket,
        source.name,
        generation,
        source.access,
    )?;
    Ok(Some(generation))
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
        let source_name = source.name.to_owned();
        let source_requested_generation = source.generation;
        let source_access = source.access.clone();
        let source_query = source_bucket.clone();
        let source_object = source_name.clone();
        let source_authority = source_access.clone();
        let token = hash(invocation.token.unwrap_or_default().as_bytes());
        let now = unix_micros(self.0.clock.now())?;
        let (source_incarnation, source_generation): (String, Option<i64>) = self
            .run(move |connection| {
                let transaction = connection.transaction()?;
                let generation = copy_source_generation(
                    &transaction,
                    CopyReplaySource {
                        bucket: &source_query,
                        name: &source_object,
                        generation: source_requested_generation,
                        access: &source_authority,
                    },
                    &token,
                    now,
                )?;
                let incarnation = bucket_incarnation(&transaction, &source_query)?;
                Ok::<_, ObjectError>((incarnation, generation))
            })
            .await?;
        let request = serde_json::to_string(&(invocation.request, &source_incarnation))
            .map_err(objects::object_unavailable)?;
        // No lease is held here, so an exclusive job may have started since the job above.
        let mut permit = self.wait_object_upload().await?;
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
            fingerprint: copy_fingerprint(&request),
            source_bucket,
            source_name,
            source_generation,
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
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            replay.cached(&transaction, unix_micros(clock.now())?)
        })
        .await
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
        let principals = self.0.principals.get().cloned();
        let replay = self
            .run(move |connection| {
                let transaction = connection.transaction()?;
                let (_, delete_permission) =
                    objects::authorized_bucket(&transaction, &bucket, &access)?;
                let incarnation = bucket_incarnation(&transaction, &bucket)?;
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
                    principals,
                };
                let now = unix_micros(clock.now())?;
                if replay.cached(&transaction, now)?.is_none() {
                    let previous = objects::lookup(&transaction, &bucket, &name)?;
                    objects::check_publication(
                        previous.as_ref(),
                        conditions,
                        delete_permission,
                        clock.now(),
                    )?;
                }
                Ok::<_, ObjectError>(replay)
            })
            .await?;
        permit.replay = Some(replay);
        Ok(())
    }
}

impl UploadGuard {
    pub(crate) fn replay_acl_snapshot(&self) -> Option<UploadAclSnapshot> {
        self.replay.as_ref().map(|r| r.snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::super::unavailable;
    use super::*;
    use crate::object::ObjectMetadataUpdate;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn copy_replay_reports_a_removed_source_bucket_as_a_precondition() {
        let catalog = Catalog::open_memory_for_server(
            1024,
            1024 * 1024,
            Arc::new(crate::clock::SystemClock),
            super::super::admin::LocalAdmin::Disabled,
        )
        .await
        .unwrap();
        for name in ["source", "destination"] {
            catalog
                .insert_bucket(crate::storage::test_fixtures::bucket(name, "project"))
                .await
                .unwrap();
        }
        let replay = catalog
            .prepare_copy_replay(
                "destination",
                "target",
                Default::default(),
                &BucketAccess::Permissive,
                CopyReplaySource {
                    bucket: "source",
                    name: "object",
                    generation: Some(1),
                    access: &BucketAccess::Permissive,
                },
                ReplayInvocation {
                    token: Some("invocation"),
                    request: "copy",
                },
            )
            .await
            .unwrap()
            .unwrap();
        catalog
            .run::<_, CatalogError>(|connection| {
                sql::exec(
                    connection,
                    "DELETE FROM bucket_incarnations WHERE bucket='source'",
                    [],
                )?;
                sql::exec(connection, "DELETE FROM buckets WHERE name='source'", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(
            catalog.copy_replay_record(&replay).await,
            Err(ObjectError::PreconditionFailed)
        );
        catalog.close().await.unwrap();
    }

    /// A table of `rows` records, each with a later deadline than the one before.
    fn replay_table(rows: i64) -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE upload_replays(token INTEGER PRIMARY KEY, deadline INTEGER); CREATE INDEX upload_replays_deadline ON upload_replays(deadline, token);")
            .unwrap();
        sql::exec(
            &connection,
            "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM ids WHERE n < ?1) INSERT INTO upload_replays SELECT n, 100 + n FROM ids",
            [rows],
        )
        .unwrap();
        connection
    }

    fn oldest_and_count(connection: &Connection) -> (i64, i64) {
        sql::row(
            connection,
            "SELECT min(token), count(*) FROM upload_replays",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn making_room_drops_only_what_the_window_needs_oldest_first() {
        // One slot left: nothing goes.
        let spare = replay_table(CAPACITY - 1);
        make_room(&spare, 0).unwrap();
        assert_eq!(oldest_and_count(&spare), (1, CAPACITY - 1));
        // Full: exactly the oldest goes.
        let full = replay_table(CAPACITY);
        make_room(&full, 0).unwrap();
        assert_eq!(oldest_and_count(&full), (2, CAPACITY - 1));
        // A catalog already over the window comes back within it rather than failing.
        let over = replay_table(CAPACITY + 5);
        make_room(&over, 0).unwrap();
        assert_eq!(oldest_and_count(&over), (7, CAPACITY - 1));
        // Expired records go first, and only they when that makes enough room.
        let expired = replay_table(CAPACITY);
        make_room(&expired, 103).unwrap();
        assert_eq!(oldest_and_count(&expired), (4, CAPACITY - 3));
    }

    #[test]
    fn completion_decoding_and_startup_reject_corrupt_enums_and_json() {
        let record = objects::PublicationDefaults {
            storage_class: crate::bucket::StorageClass::Standard,
            event_based_hold: false,
            time: std::time::UNIX_EPOCH,
            owner: None,
        }
        .record(
            crate::object::ObjectTarget::new("bucket", "object", 1),
            "text/plain".into(),
            0,
            crate::hashing::Hashes {
                md5: [0; 16],
                crc32c: 0,
            },
        );
        let original =
            serde_json::json!({"record":record,"acl":[{"entity":"allUsers","role":"READER"}]});
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE upload_replays(token TEXT,completion TEXT);")
            .unwrap();
        for field in ["role", "storage_class", "json"] {
            let mut value = original.clone();
            let stored = match field {
                "role" => {
                    value["acl"][0]["role"] = serde_json::json!("UNKNOWN");
                    value.to_string()
                }
                "storage_class" => {
                    value["record"]["storage_class"] = serde_json::json!("UNKNOWN");
                    value.to_string()
                }
                _ => "not-json".into(),
            };
            let error = decode_completion(&stored, 3).err().unwrap();
            assert!(
                matches!(unavailable(error), CatalogError::Corrupt(_)),
                "{field}"
            );
            connection
                .execute("DELETE FROM upload_replays", [])
                .unwrap();
            connection
                .execute("INSERT INTO upload_replays VALUES ('token',?1)", [&stored])
                .unwrap();
            assert!(
                matches!(validate(&connection), Err(CatalogError::Corrupt(_))),
                "{field}"
            );
        }
    }

    /// Replay identities survive restarts and upgrades; their versioned encoding is fixed.
    #[test]
    fn fingerprint_encoding_has_a_stable_golden_digest() {
        let metadata = ObjectMetadataUpdate {
            content_type: Some(Some("text/plain".into())),
            metadata: Some(Some(BTreeMap::from([
                ("zeta".to_owned(), Some("1".to_owned())),
                ("alpha".to_owned(), None),
                ("mid".to_owned(), Some("3".to_owned())),
            ]))),
            acl: Some(vec![
                crate::acl::AclEntry {
                    entity: "user-b@example.test".into(),
                    role: crate::acl::AclRole::Owner,
                },
                crate::acl::AclEntry {
                    entity: "allUsers".into(),
                    role: crate::acl::AclRole::Reader,
                },
            ]),
            override_unlocked_retention: crate::object::retention::UnlockedOverride::Requested,
            ..super::super::resumable::golden_tests::populated_options().metadata
        };
        let request = UploadFingerprint {
            bucket: "bucket",
            name: "name",
            content_type: "text/plain",
            metadata: &metadata,
            conditions: ObjectConditions {
                if_generation_match: Some(7),
                if_generation_not_match: Some(8),
                if_metageneration_match: Some(9),
                if_metageneration_not_match: Some(10),
            },
            size: 5,
            sha256: [7; 32],
        };
        let digest = request.digest();
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            "9800c73b1029a34e7809af2ace71da8e6668531db720b33ce2f448732d4735d7"
        );
        let mut replace = metadata.clone();
        replace.metadata_mode = crate::object::MapMode::Replace;
        assert_ne!(
            digest,
            UploadFingerprint {
                metadata: &replace,
                ..request
            }
            .digest()
        );
    }
}
