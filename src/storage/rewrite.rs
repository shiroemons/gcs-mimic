use super::{
    Catalog, CatalogError,
    derived::{DerivedDestination, DerivedSegment},
    resumable,
    rewrite_token::RewriteToken,
    system_time_millis, unix_millis,
};
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::SystemTime,
};

pub(crate) use super::rewrite_token::RewriteBinding;

pub(super) const SCHEMA: &str = "CREATE TABLE rewrite_sessions (
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    token TEXT NOT NULL UNIQUE CHECK(length(token) = 64),
    previous_token TEXT UNIQUE CHECK(previous_token IS NULL OR length(previous_token) = 64),
    request TEXT NOT NULL CHECK(length(CAST(request AS BLOB)) <= 262144),
    offset INTEGER NOT NULL CHECK(offset >= 0),
    total INTEGER NOT NULL CHECK(total >= offset),
    deadline INTEGER NOT NULL,
    response TEXT CHECK(length(CAST(response AS BLOB)) <= 65536),
    transfer_id TEXT UNIQUE REFERENCES derived_transfers(id) ON DELETE CASCADE
        CHECK(transfer_id IS NULL OR transfer_id = id)
) WITHOUT ROWID;
CREATE INDEX rewrite_sessions_deadline ON rewrite_sessions(deadline, id);
PRAGMA user_version = 11;";

#[derive(Debug)]
pub(crate) enum RewriteError {
    Invalid,
    Gone,
    Object(ObjectError),
}
impl From<ObjectError> for RewriteError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::DerivedTransferNotFound | ObjectError::DerivedTransferExpired => {
                Self::Gone
            }
            other => Self::Object(other),
        }
    }
}
impl From<CatalogError> for RewriteError {
    fn from(error: CatalogError) -> Self {
        Self::Object(unavailable(error))
    }
}
fn unavailable(error: impl std::fmt::Display) -> ObjectError {
    ObjectError::Unavailable(error.to_string())
}

pub(crate) struct RewriteStart {
    pub binding: RewriteBinding,
    pub destination: DerivedDestination,
    pub source_conditions: ObjectConditions,
    pub source_metageneration: i64,
    pub source_access: BucketAccess,
    pub budget: u64,
}

#[derive(Clone)]
pub(crate) struct RewriteSession {
    pub id: String,
    pub token: String,
    pub previous_token: Option<String>,
    pub binding: RewriteBinding,
    pub destination: DerivedDestination,
    source_conditions: ObjectConditions,
    source_metageneration: i64,
    source_incarnation: String,
    destination_incarnation: String,
    budget: u64,
    pub offset: u64,
    pub total: u64,
    deadline: i64,
    response: Option<Value>,
}

#[derive(Debug)]
pub(crate) struct RewriteProgress {
    pub total: u64,
    pub offset: u64,
    pub token: Option<String>,
    pub resource: Option<Value>,
    pub fields: Option<String>,
}

fn incarnation(connection: &Connection, bucket: &str) -> Result<String, ObjectError> {
    connection
        .query_row(
            "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
            [bucket],
            |row| row.get(0),
        )
        .optional()
        .map_err(unavailable)?
        .ok_or(ObjectError::BucketNotFound)
}

fn encode(start: &RewriteStart, source: &str, destination: &str) -> Value {
    let binding = &start.binding;
    let options = resumable::ResumableOptions {
        content_type: start.destination.content_type.clone(),
        metadata: start.destination.metadata.clone(),
        expected: start.destination.expected,
        conditions: start.destination.conditions,
        ..Default::default()
    };
    let conditions = start.source_conditions;
    json!({"path":[binding.source_bucket,binding.source_object,binding.destination_bucket,binding.destination_object],
        "generation":binding.source_generation,"metageneration":start.source_metageneration,
        "parameters":binding.parameters,"body":binding.metadata,"source_incarnation":source,"destination_incarnation":destination,
        "options":resumable::encode_options(&options),"class":start.destination.storage_class,"components":start.destination.component_count,
        "source_conditions":[conditions.if_generation_match,conditions.if_generation_not_match,conditions.if_metageneration_match,conditions.if_metageneration_not_match],"budget":start.budget})
}

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<RewriteSession> {
    let decode = || -> Result<RewriteSession, String> {
        let request: Value =
            serde_json::from_str(&row.get::<_, String>(3).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let options = resumable::decode_options(&request["options"])?;
        let string = |value: &Value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| "invalid rewrite string".to_owned())
        };
        let integer = |value: &Value| {
            value
                .as_i64()
                .ok_or_else(|| "invalid rewrite integer".to_owned())
        };
        let values: Vec<Option<i64>> = serde_json::from_value(request["source_conditions"].clone())
            .map_err(|e| e.to_string())?;
        let values: [Option<i64>; 4] = values
            .try_into()
            .map_err(|_| "invalid rewrite conditions")?;
        let get = |index| row.get(index).map_err(|e| e.to_string());
        let body = request.get("body").filter(|v| !v.is_null()).cloned();
        let session = RewriteSession {
            id: get(0)?,
            token: get(1)?,
            previous_token: row.get(2).map_err(|e| e.to_string())?,
            binding: RewriteBinding {
                source_bucket: string(&request["path"][0])?,
                source_object: string(&request["path"][1])?,
                destination_bucket: string(&request["path"][2])?,
                destination_object: string(&request["path"][3])?,
                source_generation: integer(&request["generation"])?,
                parameters: request["parameters"]
                    .as_object()
                    .cloned()
                    .ok_or("invalid rewrite parameters")?,
                metadata: body,
            },
            destination: DerivedDestination {
                component_count: serde_json::from_value(request["components"].clone())
                    .map_err(|e| e.to_string())?,
                bucket: string(&request["path"][2])?,
                name: string(&request["path"][3])?,
                content_type: options.content_type,
                metadata: options.metadata,
                storage_class: serde_json::from_value(request["class"].clone())
                    .map_err(|e| e.to_string())?,
                expected: options.expected,
                conditions: options.conditions,
                access: BucketAccess::Unchecked,
            },
            source_conditions: ObjectConditions {
                if_generation_match: values[0],
                if_generation_not_match: values[1],
                if_metageneration_match: values[2],
                if_metageneration_not_match: values[3],
            },
            source_metageneration: integer(&request["metageneration"])?,
            source_incarnation: string(&request["source_incarnation"])?,
            destination_incarnation: string(&request["destination_incarnation"])?,
            budget: request["budget"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or("invalid rewrite budget")?,
            offset: row.get::<_, i64>(4).map_err(|e| e.to_string())? as u64,
            total: row.get::<_, i64>(5).map_err(|e| e.to_string())? as u64,
            deadline: row.get(6).map_err(|e| e.to_string())?,
            response: row
                .get::<_, Option<String>>(7)
                .map_err(|e| e.to_string())?
                .map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                .transpose()?,
        };
        let opaque_id = |value: &str| {
            value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        let transfer_id: Option<String> = row.get(8).map_err(|error| error.to_string())?;
        let completed = session
            .response
            .as_ref()
            .and_then(|response| response.get("resource"))
            .is_some_and(Value::is_object);
        let response_valid = match &session.response {
            None => session.offset == 0 && session.previous_token.is_none(),
            Some(response) => {
                response["offset"].as_u64() == Some(session.offset)
                    && if response.get("resource").is_some_and(Value::is_object) {
                        session.offset == session.total && response.get("token").is_none()
                    } else {
                        session.offset < session.total
                            && response["token"].as_str() == Some(session.token.as_str())
                    }
            }
        };
        if !opaque_id(&session.id)
            || if completed {
                transfer_id.is_some()
            } else {
                transfer_id.as_deref() != Some(session.id.as_str())
            }
            || !opaque_id(&session.source_incarnation)
            || !opaque_id(&session.destination_incarnation)
            || !response_valid
            || session
                .previous_token
                .as_ref()
                .is_some_and(|token| RewriteToken::parse(token).is_none())
            || RewriteToken::parse(&session.token).is_none()
            || session.binding.source_generation <= 0
            || session.source_metageneration <= 0
            || session.offset > session.total
        {
            return Err("invalid rewrite session".into());
        }
        Ok(session)
    };
    decode().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, error.into())
    })
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection
        .query_row("SELECT (SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NOT NULL)<=128 AND (SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NULL)<=128", [], |row| {
            row.get(0)
        })
        .map_err(super::unavailable)?;
    if !valid {
        return Err(super::unavailable("rewrite session limit exceeded"));
    }
    let mut statement = connection
        .prepare("SELECT * FROM rewrite_sessions ORDER BY id LIMIT 257")
        .map_err(super::unavailable)?;
    let mut rows = statement.query([]).map_err(super::unavailable)?;
    while let Some(row) = rows.next().map_err(super::unavailable)? {
        read(row).map_err(super::unavailable)?;
    }
    Ok(())
}

pub(super) fn recover_due(connection: &mut Connection, now: i64) -> Result<(), CatalogError> {
    let due: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM rewrite_sessions WHERE deadline<=?1)",
            [now],
            |row| row.get(0),
        )
        .map_err(super::unavailable)?;
    if !due {
        return Ok(());
    }
    let transaction = connection.transaction().map_err(super::unavailable)?;
    transaction.execute("INSERT OR IGNORE INTO pending_blob_deletions SELECT transfer_id FROM rewrite_sessions WHERE deadline<=?1 AND transfer_id IS NOT NULL",[now]).map_err(super::unavailable)?;
    transaction.execute("DELETE FROM derived_inputs WHERE transfer_id IN (SELECT id FROM rewrite_sessions WHERE deadline<=?1)",[now]).map_err(super::unavailable)?;
    transaction.execute("DELETE FROM derived_transfers WHERE id IN (SELECT id FROM rewrite_sessions WHERE deadline<=?1)",[now]).map_err(super::unavailable)?;
    transaction
        .execute("DELETE FROM rewrite_sessions WHERE deadline<=?1", [now])
        .map_err(super::unavailable)?;
    transaction.commit().map_err(super::unavailable)
}

fn check_source(
    connection: &Connection,
    session: &RewriteSession,
    access: &BucketAccess,
) -> Result<ObjectRecord, ObjectError> {
    check_incarnations(connection, session)?;
    let generation = session
        .binding
        .parameters
        .get("sourceGeneration")
        .map(|_| session.binding.source_generation);
    let source = Catalog::check_derived_source_in_transaction(
        connection,
        &session.binding.source_bucket,
        &session.binding.source_object,
        generation,
        session.source_conditions,
        access,
    )
    .map_err(|error| match error {
        ObjectError::ObjectNotFound | ObjectError::BucketNotFound => {
            ObjectError::PreconditionFailed
        }
        other => other,
    })?;
    if source.generation != session.binding.source_generation
        || source.metageneration != session.source_metageneration
    {
        return Err(ObjectError::PreconditionFailed);
    }
    Ok(source)
}

fn check_incarnations(
    connection: &Connection,
    session: &RewriteSession,
) -> Result<(), ObjectError> {
    if incarnation(connection, &session.binding.source_bucket)? != session.source_incarnation
        || incarnation(connection, &session.binding.destination_bucket)?
            != session.destination_incarnation
    {
        return Err(ObjectError::PreconditionFailed);
    }
    Ok(())
}

fn progress(session: &RewriteSession, response: Value) -> Result<RewriteProgress, ObjectError> {
    Ok(RewriteProgress {
        total: session.total,
        offset: response["offset"]
            .as_u64()
            .ok_or_else(|| unavailable("invalid rewrite response"))?,
        token: response["token"].as_str().map(str::to_owned),
        resource: response.get("resource").filter(|v| !v.is_null()).cloned(),
        fields: session
            .binding
            .parameters
            .get("fields")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

impl Catalog {
    pub(crate) async fn begin_rewrite_session(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
        start: RewriteStart,
    ) -> Result<(String, super::derived::DerivedRequestGuard), RewriteError> {
        let token = RewriteToken::generate()
            .map_err(unavailable)?
            .as_str()
            .to_owned();
        let result = token.clone();
        // shortcut: crash before delivery leaves durable state until expiry; add a delivered marker for eager recovery.
        let owner = Arc::new(Mutex::new(None));
        let callback_owner = owner.clone();
        let catalog = self.clone();
        self.begin_derived_with_binding(segments, max_bytes, false, move |connection, transfer| {
            *callback_owner.lock().map_err(unavailable)? =
                Some(catalog.guard_derived_request(&transfer.id));
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NOT NULL",
                    [],
                    |row| row.get(0),
                )
                .map_err(unavailable)?;
            if count >= 128 {
                return Err(unavailable("rewrite session limit reached"));
            }
            let request = encode(
                &start,
                &incarnation(connection, &start.binding.source_bucket)?,
                &incarnation(connection, &start.binding.destination_bucket)?,
            );
            connection
                .execute(
                    "INSERT INTO rewrite_sessions VALUES (?1,?2,NULL,?3,0,?4,?5,NULL,?1)",
                    params![
                        transfer.id,
                        token,
                        request.to_string(),
                        transfer.total as i64,
                        unix_millis(transfer.deadline).map_err(unavailable)?
                    ],
                )
                .map_err(unavailable)?;
            let session = connection
                .query_row(
                    "SELECT * FROM rewrite_sessions WHERE id=?1",
                    [&transfer.id],
                    read,
                )
                .map_err(unavailable)?;
            check_source(connection, &session, &start.source_access).map(|_| ())
        })
        .await?;
        self.0.maintenance_wake.notify_one();
        let owner = owner
            .lock()
            .map_err(unavailable)?
            .take()
            .ok_or_else(|| unavailable("missing rewrite request owner"))?;
        Ok((result, owner))
    }

    pub(crate) async fn rewrite_session(
        &self,
        token: &str,
    ) -> Result<RewriteSession, RewriteError> {
        let token = RewriteToken::parse(token)
            .ok_or(RewriteError::Invalid)?
            .as_str()
            .to_owned();
        let now = unix_millis(self.0.clock.now())?;
        let session = self
            .run_object(move |connection| {
                connection
                    .query_row(
                        "SELECT * FROM rewrite_sessions WHERE token = ?1 OR previous_token = ?1",
                        [token],
                        read,
                    )
                    .optional()
                    .map_err(unavailable)
            })
            .await?
            .ok_or(RewriteError::Gone)?;
        if session.deadline <= now {
            return Err(RewriteError::Gone);
        }
        Ok(session)
    }

    pub(crate) async fn continue_rewrite(
        &self,
        token: &str,
        source_access: BucketAccess,
        destination_access: BucketAccess,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<RewriteProgress, RewriteError> {
        let session = self.rewrite_session(token).await?;
        let _guard = self.try_resumable_writer(&format!("rewrite:{}", session.id))?;
        let mut session = self.rewrite_session(token).await?;
        let replay = session.previous_token.as_deref() == Some(token);
        let checked = session.clone();
        let source_check = source_access.clone();
        let destination_check = destination_access.clone();
        let source_result = self
            .run_object(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                if replay {
                    check_incarnations(&transaction, &checked)?;
                    super::objects::authorized_bucket(
                        &transaction,
                        &checked.binding.source_bucket,
                        &source_check,
                    )?;
                    super::objects::authorized_bucket(
                        &transaction,
                        &checked.binding.destination_bucket,
                        &destination_check,
                    )?;
                } else {
                    check_source(&transaction, &checked, &source_check)?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = source_result {
            if session
                .response
                .as_ref()
                .is_none_or(|value| !value["resource"].is_object())
                && matches!(
                    error,
                    ObjectError::PreconditionFailed
                        | ObjectError::BucketNotFound
                        | ObjectError::ObjectNotFound
                )
                && let Err(cleanup) = self.cancel_derived(&session.id).await
            {
                eprintln!("terminal rewrite cleanup failed: {cleanup}");
            }
            return Err(error.into());
        }
        if replay {
            return progress(
                &session,
                session
                    .response
                    .clone()
                    .ok_or_else(|| unavailable("missing rewrite replay"))?,
            )
            .map_err(Into::into);
        }
        let current = self.get_derived(&session.id).await?;
        let target = session
            .offset
            .saturating_add(session.budget)
            .min(session.total);
        let current = self
            .advance_derived(&session.id, target.saturating_sub(current.offset))
            .await?;
        if current.offset == current.total {
            session.destination.access = destination_access;
            let destination = session.destination.clone();
            let id = session.id.clone();
            let clock = self.0.clock.clone();
            let validate_session = session.clone();
            let completed_session = session.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            // shortcut: publish-time permission failures discard progress; specialize cleanup when retry must survive revocation.
            self.publish_derived_prepared(&id,destination,move |connection,record| {
                if validate_session.deadline<=unix_millis(clock.now()).map_err(unavailable)? {return Err(ObjectError::DerivedTransferExpired);}
                let source = check_source(connection,&validate_session,&source_access)?;
                let same_location: bool = connection.query_row("SELECT source.location=destination.location FROM buckets source JOIN buckets destination ON destination.name=?2 WHERE source.name=?1",params![source.bucket,record.bucket],|row|row.get(0)).map_err(unavailable)?;
                record.component_count = crate::json::derived_component_count(&source,&record.storage_class,same_location);
                Ok(())
            },move |connection,record| {
                let response=json!({"offset":record.size,"resource":render(record)?});
                connection.execute("UPDATE rewrite_sessions SET previous_token = token, offset = total, response = ?2, transfer_id = NULL WHERE id = ?1",params![session.id,response.to_string()]).map_err(unavailable)?;
                // shortcut: eviction follows creation deadlines; add completion ordering for recent-finish retention.
                connection.execute("DELETE FROM rewrite_sessions WHERE id IN (SELECT id FROM rewrite_sessions WHERE transfer_id IS NULL AND id!=?1 ORDER BY deadline,id LIMIT max((SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NULL)-128,0))",[&session.id]).map_err(unavailable)?;
                // The stored response remains authoritative if its network receiver disappears.
                let _=sender.send(response);
                Ok(())
            },None).await?;
            return progress(&completed_session, receiver.await.map_err(unavailable)?)
                .map_err(Into::into);
        }
        let next = RewriteToken::generate()
            .map_err(unavailable)?
            .as_str()
            .to_owned();
        let response = json!({"offset":current.offset,"token":next});
        let saved = response.to_string();
        let id = session.id.clone();
        let now = unix_millis(self.0.clock.now())?;
        self.run_object(move |connection| {
            let changed=connection.execute("UPDATE rewrite_sessions SET previous_token=token, token=?2, offset=?3, response=?4 WHERE id=?1 AND deadline>?5",params![id,next,current.offset as i64,saved,now]).map_err(unavailable)?;
            if changed!=1 {return Err(ObjectError::PreconditionFailed);}Ok(())
        }).await?;
        progress(&session, response).map_err(Into::into)
    }

    pub(crate) async fn maintain_rewrite_sessions(
        &self,
        now: i64,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let mut after = String::new();
        loop {
            let cursor = after.clone();
            let ids=self.run(move |connection| connection.prepare("SELECT id FROM rewrite_sessions WHERE deadline<=?1 AND id>?2 ORDER BY id LIMIT 16").map_err(super::unavailable)?.query_map(params![now,cursor],|row|row.get::<_,String>(0)).map_err(super::unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(super::unavailable)).await?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                after = id.clone();
                let key = format!("rewrite:{id}");
                let _guard = {
                    let mut writers = self.0.session_writers.lock().map_err(super::unavailable)?;
                    if writers.contains(&key) || writers.len() >= 128 {
                        continue;
                    }
                    writers.insert(key.clone());
                    resumable::WriterGuard {
                        catalog: self.clone(),
                        id: key,
                    }
                };
                match self.cancel_derived(&id).await {
                    Ok(()) | Err(ObjectError::DerivedTransferNotFound) => {}
                    Err(error) => return Err(super::unavailable(error)),
                }
                self.run(move |connection| {
                    connection
                        .execute(
                            "DELETE FROM rewrite_sessions WHERE id=?1 AND deadline<=?2",
                            params![id, now],
                        )
                        .map(|_| ())
                        .map_err(super::unavailable)
                })
                .await?;
            }
        }
        self.run(move |connection| {
            let deadline: Option<i64> = connection
                .query_row(
                    "SELECT min(deadline) FROM rewrite_sessions WHERE deadline>?1",
                    [now],
                    |row| row.get(0),
                )
                .map_err(super::unavailable)?;
            deadline
                .map(system_time_millis)
                .transpose()
                .map_err(super::unavailable)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bucket::BucketRecord,
        clock::Clock,
        object::{ExpectedHashes, UploadChunk},
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, UNIX_EPOCH},
    };

    struct TestClock(AtomicU64);
    impl Clock for TestClock {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_millis(self.0.load(Ordering::SeqCst))
        }
    }

    async fn fixture() -> (Catalog, std::path::PathBuf, Arc<TestClock>) {
        let directory = std::env::temp_dir().join(format!(
            "gcs-mimic-rewrite-{}",
            RewriteToken::generate().unwrap().as_str()
        ));
        let clock = Arc::new(TestClock(AtomicU64::new(1000)));
        let catalog = Catalog::open_with_clock(&directory, clock.clone())
            .await
            .unwrap();
        catalog
            .insert_bucket(BucketRecord {
                name: "bucket".into(),
                project: "project".into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                public_access_prevention_enforced: false,
                cors: Vec::new(),
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        sender.send(Ok(UploadChunk::End)).await.unwrap();
        drop(sender);
        catalog
            .put_object(
                "bucket",
                "source",
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                8,
                receiver,
            )
            .await
            .unwrap();
        (catalog, directory, clock)
    }

    async fn new_session(
        catalog: &Catalog,
    ) -> Result<(String, super::super::derived::DerivedRequestGuard), RewriteError> {
        let source = catalog
            .resolve_derived_source(
                "bucket",
                "source",
                None,
                ObjectConditions::default(),
                &BucketAccess::Unchecked,
            )
            .await
            .unwrap();
        let start = RewriteStart {
            binding: RewriteBinding {
                source_bucket: "bucket".into(),
                source_object: "source".into(),
                source_generation: source.object.generation,
                destination_bucket: "bucket".into(),
                destination_object: "destination".into(),
                parameters: Default::default(),
                metadata: None,
            },
            destination: DerivedDestination {
                bucket: "bucket".into(),
                name: "destination".into(),
                content_type: "text/plain".into(),
                ..Default::default()
            },
            source_conditions: ObjectConditions::default(),
            source_metageneration: source.object.metageneration,
            source_access: BucketAccess::Unchecked,
            budget: 1048576,
        };
        catalog
            .begin_rewrite_session(vec![source.whole()], 8, start)
            .await
    }

    #[tokio::test]
    async fn rewrite_active_sessions_are_bounded_and_busy_expiry_does_not_block_others() {
        let (catalog, directory, clock) = fixture().await;
        let mut first = None;
        for index in 0..=128 {
            let result = new_session(&catalog).await;
            if index == 128 {
                assert!(matches!(
                    result,
                    Err(RewriteError::Object(ObjectError::Unavailable(_)))
                ));
                break;
            }
            let (token, mut owner) = result.unwrap();
            owner.disarm();
            first.get_or_insert(token);
        }
        let token = first.unwrap();
        let session = catalog.rewrite_session(&token).await.unwrap();
        let guard = catalog
            .try_resumable_writer(&format!("rewrite:{}", session.id))
            .unwrap();
        clock.0.store(604801000, Ordering::SeqCst);
        assert!(matches!(
            catalog.rewrite_session(&token).await,
            Err(RewriteError::Gone)
        ));
        assert!(
            catalog
                .maintain_rewrite_sessions(604801000)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            catalog
                .run(|connection| connection
                    .query_row("SELECT count(*) FROM rewrite_sessions", [], |row| row
                        .get::<_, i64>(0))
                    .map_err(super::super::unavailable))
                .await
                .unwrap(),
            1
        );
        catalog.0.maintenance_wake.notified().await;
        drop(guard);
        tokio::time::timeout(
            Duration::from_millis(100),
            catalog.0.maintenance_wake.notified(),
        )
        .await
        .unwrap();
        assert!(
            catalog
                .maintain_rewrite_sessions(604801000)
                .await
                .unwrap()
                .is_none()
        );
        let source = catalog
            .get_object("bucket", "source", None, ObjectConditions::default())
            .await
            .unwrap();
        assert_eq!(source.size, 0);
        assert_eq!(
            catalog
                .run(|connection| connection
                    .query_row("SELECT count(*) FROM rewrite_sessions", [], |row| row
                        .get::<_, i64>(0))
                    .map_err(super::super::unavailable))
                .await
                .unwrap(),
            0
        );
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn rewrite_committed_completion_does_not_reread_an_expired_token() {
        let (catalog, directory, clock) = fixture().await;
        let (token, mut owner) = new_session(&catalog).await.unwrap();
        owner.disarm();
        let expiry_clock = clock.clone();
        let result = catalog
            .continue_rewrite(
                &token,
                BucketAccess::Unchecked,
                BucketAccess::Unchecked,
                move |record| {
                    expiry_clock.0.store(604801000, Ordering::SeqCst);
                    Ok(json!({"generation":record.generation}))
                },
            )
            .await
            .unwrap();
        assert!(result.resource.is_some());
        assert!(matches!(
            catalog.rewrite_session(&token).await,
            Err(RewriteError::Gone)
        ));
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open_with_clock(&directory, clock).await.unwrap();
        assert_eq!(
            catalog
                .get_object("bucket", "destination", None, ObjectConditions::default())
                .await
                .unwrap()
                .size,
            0
        );
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn rewrite_initial_request_owner_reclaims_undelivered_state() {
        let (catalog, directory, _clock) = fixture().await;
        let (_token, owner) = new_session(&catalog).await.unwrap();
        drop(owner);
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let count = catalog
                    .run(|connection| {
                        connection
                            .query_row("SELECT count(*) FROM rewrite_sessions", [], |row| {
                                row.get::<_, i64>(0)
                            })
                            .map_err(super::super::unavailable)
                    })
                    .await
                    .unwrap();
                if count == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_dir(directory.join("staging"))
                .unwrap()
                .count(),
            0
        );
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
