use super::{
    Catalog, CatalogError, UnixMillis,
    derived::{DerivedDestination, DerivedSegment},
    resumable,
    resumable::writers::{ClaimError, KeyOrigin, WriterKey},
    resumable_maintenance::SweepOutcome,
    rewrite_token::RewriteToken,
    session_limits::{MAX_TRIMMED_AT_ONCE, finished_excess},
};
use crate::cause::BackendFailure;
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::{Incarnation, bucket_incarnation, sql};
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord, ObjectTarget},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{num::NonZeroU64, time::SystemTime};

pub(crate) use super::rewrite_token::{RewriteBinding, RewriteNames};

#[cfg(test)]
mod golden_tests;

/// Names the expired-session sweep in the maintenance log.
const REWRITE_SWEEP: &str = "rewrite session";

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

macro_rules! session_columns {
    () => {
        "id, token, previous_token, request, offset, total, deadline, response, transfer_id"
    };
}

fn read_session(
    connection: &Connection,
    token: &RewriteToken,
    now: UnixMillis,
) -> Result<Option<RewriteSession>, ObjectError> {
    let session = sql::row(
        connection,
        concat!(
            "SELECT ",
            session_columns!(),
            " FROM rewrite_sessions WHERE token=?1 OR previous_token=?1"
        ),
        [token],
        read,
    )
    .optional()
    .map_err(object_unavailable)?;
    Ok(session.filter(|session| session.deadline > now))
}

fn check_continuation(
    connection: &Connection,
    session: &RewriteSession,
    token: &RewriteToken,
    access: (&BucketAccess, &BucketAccess),
    now: UnixMillis,
) -> Result<Option<super::derived::DerivedTransfer>, ObjectError> {
    if session.previous_token.as_ref() == Some(token) {
        check_incarnations(connection, session)?;
        super::objects::authorize_generation(
            connection,
            &session.binding.source.bucket,
            &session.binding.source.name,
            session.binding.source.generation,
            access.0,
        )?;
        super::objects::authorized_bucket(
            connection,
            &session.binding.destination.bucket,
            access.1,
        )?;
        Ok(None)
    } else {
        check_source(connection, session, access.0)?;
        super::derived::transfer(connection, &session.id, now).map(|(transfer, _)| Some(transfer))
    }
}

#[derive(Debug)]
pub(crate) enum RewriteError {
    Invalid,
    Gone,
    Object(ObjectError),
}
impl From<ObjectError> for RewriteError {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every other object error is carried unchanged"
    )]
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
        Self::Object(error.into())
    }
}
use crate::storage::failure::object_unavailable;

/// Copies the rest of the object in one rewrite call: the step target saturates to its size.
pub(crate) const UNLIMITED_REWRITE_BUDGET: NonZeroU64 = NonZeroU64::MAX;
/// Bytes one call of a rewrite that needs several calls copies when the request sets no limit.
pub(crate) const DEFAULT_REWRITE_BUDGET: NonZeroU64 = NonZeroU64::new(8 * 1024 * 1024).unwrap();

/// The bucket identities and placement observed together when choosing a rewrite budget.
pub(crate) struct RewriteBuckets {
    access: BucketAccess,
    conditions: ObjectConditions,
    source: Incarnation,
    destination: Incarnation,
    same_location: bool,
    destination_class: crate::bucket::StorageClass,
}

impl RewriteBuckets {
    fn keeps_placement(
        &self,
        source: crate::bucket::StorageClass,
        requested: Option<crate::bucket::StorageClass>,
    ) -> bool {
        self.same_location && requested.unwrap_or(self.destination_class) == source
    }

    pub(crate) fn budget(
        &self,
        source: crate::bucket::StorageClass,
        requested: Option<crate::bucket::StorageClass>,
        limit: Option<NonZeroU64>,
    ) -> NonZeroU64 {
        if self.keeps_placement(source, requested) {
            UNLIMITED_REWRITE_BUDGET
        } else {
            limit.unwrap_or(DEFAULT_REWRITE_BUDGET)
        }
    }
}

/// The lookup supplies the lease key. The guarded read authorizes the step.
pub(crate) struct RewriteStep {
    id: BlobId,
    token: RewriteToken,
    pin: Option<super::derived::PublicationPin>,
}

pub(crate) struct RewriteStart {
    pub binding: RewriteBinding,
    pub destination: DerivedDestination,
    pub source_conditions: ObjectConditions,
    pub source_metageneration: i64,
    pub source_access: BucketAccess,
    /// Verified when the session starts; every continuation must repeat the same source key
    /// (`RewriteSession::check_customer_keys`).
    pub source_key_sha256: Option<[u8; 32]>,
    /// Bytes one rewrite call copies at most.
    pub budget: NonZeroU64,
}

#[derive(Clone)]
pub(crate) struct RewriteSession {
    pub id: BlobId,
    pub token: RewriteToken,
    pub previous_token: Option<RewriteToken>,
    pub binding: RewriteBinding,
    pub destination: DerivedDestination,
    source_conditions: ObjectConditions,
    source_metageneration: i64,
    source_incarnation: Incarnation,
    destination_incarnation: Incarnation,
    pub source_key_sha256: Option<[u8; 32]>,
    budget: NonZeroU64,
    pub offset: u64,
    pub total: u64,
    deadline: UnixMillis,
    response: Option<StoredResponse>,
}

impl RewriteSession {
    /// A continuation must repeat the keys the session started with: the stored fingerprints
    /// never change, so checking them outside the copying transaction cannot race.
    pub(crate) fn check_customer_keys(
        &self,
        keys: crate::csek::CopyKeys,
    ) -> Result<(), crate::csek::CsekError> {
        crate::csek::check_data(self.source_key_sha256, keys.source)?;
        crate::csek::check_data(
            self.destination.metadata.customer_key_sha256,
            keys.destination,
        )
    }
}

/// What a rewrite step stored for replay under its previous token. Every release has written
/// exactly these two shapes: `{"offset":N,"token":T}` and `{"offset":N,"resource":{..}}`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
enum StoredResponse {
    /// Copying continues with `token`.
    Progress { offset: u64, token: RewriteToken },
    /// The destination was published. `resource` is the response the adapter rendered: a JSON
    /// object resource, or gRPC's `{"proto":[..]}`.
    Complete { offset: u64, resource: Value },
}

impl StoredResponse {
    fn offset(&self) -> u64 {
        match self {
            Self::Progress { offset, .. } | Self::Complete { offset, .. } => *offset,
        }
    }
}

/// A stored rewrite session whose columns disagree, reported at the request column as corrupt.
#[derive(Debug)]
struct InvalidRewriteSession;

impl std::fmt::Display for InvalidRewriteSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid rewrite session")
    }
}

impl std::error::Error for InvalidRewriteSession {}

/// The rules sessions have always been checked by: a `resource` object makes a completed
/// response, otherwise a string `token` makes one in progress.
impl TryFrom<Value> for StoredResponse {
    type Error = InvalidRewriteSession;

    fn try_from(value: Value) -> Result<Self, InvalidRewriteSession> {
        let Value::Object(mut fields) = value else {
            return Err(InvalidRewriteSession);
        };
        let offset = fields
            .get("offset")
            .and_then(Value::as_u64)
            .ok_or(InvalidRewriteSession)?;
        match (fields.remove("resource"), fields.get("token")) {
            (Some(resource @ Value::Object(_)), None) => Ok(Self::Complete { offset, resource }),
            (Some(Value::Object(_)), Some(_)) => Err(InvalidRewriteSession),
            // A token that is not canonical hex cannot be the session's token, which is all
            // the check asks; the parse error adds nothing to "invalid rewrite session".
            (_, Some(Value::String(token))) => token
                .parse()
                .map(|token| Self::Progress { offset, token })
                .map_err(|_| InvalidRewriteSession),
            _ => Err(InvalidRewriteSession),
        }
    }
}

#[derive(Debug)]
pub(crate) struct RewriteProgress {
    pub total: u64,
    pub offset: u64,
    pub token: Option<String>,
    pub resource: Option<Value>,
    pub fields: Option<String>,
}

fn incarnation(connection: &Connection, bucket: &str) -> Result<Incarnation, ObjectError> {
    bucket_incarnation(connection, bucket)
        .optional()?
        .ok_or(ObjectError::BucketNotFound)
}

#[derive(Serialize, Deserialize)]
struct StoredRequest {
    path: [String; 4],
    generation: i64,
    metageneration: i64,
    parameters: serde_json::Map<String, Value>,
    body: Option<Value>,
    source_incarnation: Incarnation,
    destination_incarnation: Incarnation,
    options: Value,
    class: Option<crate::bucket::StorageClass>,
    components: Option<i64>,
    source_conditions: [Option<i64>; 4],
    budget: u64,
    source_key_sha256: Option<[u8; 32]>,
}

fn encode(
    start: &RewriteStart,
    source: Incarnation,
    destination: Incarnation,
) -> Result<String, ObjectError> {
    let binding = &start.binding;
    // A rewrite checks the digests a client declares before its session starts and binds them
    // to the token, so the session keeps only those its copied bytes must reproduce.
    let options = resumable::ResumableOptions {
        content_type: start.destination.content_type.clone(),
        metadata: start.destination.metadata.clone(),
        expected: start.destination.recorded,
        conditions: start.destination.conditions,
        ..crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive)
    };
    let conditions = start.source_conditions;
    serde_json::to_string(&StoredRequest {
        path: [
            binding.source.bucket.clone(),
            binding.source.name.clone(),
            binding.destination.bucket.clone(),
            binding.destination.name.clone(),
        ],
        generation: binding.source.generation,
        metageneration: start.source_metageneration,
        parameters: binding.parameters.clone(),
        body: binding.metadata.clone(),
        source_incarnation: source,
        destination_incarnation: destination,
        options: resumable::encode_options(&options)?,
        class: start.destination.storage_class,
        components: start.destination.component_count,
        source_conditions: [
            conditions.if_generation_match,
            conditions.if_generation_not_match,
            conditions.if_metageneration_match,
            conditions.if_metageneration_not_match,
        ],
        budget: start.budget.get(),
        source_key_sha256: start.source_key_sha256,
    })
    .map_err(object_unavailable)
}

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<RewriteSession> {
    let request: StoredRequest = row.get::<_, sql::Json<_>>(3)?.0;
    let options = resumable::decode_options(&request.options)
        .map_err(|error| sql::conversion_error(3, rusqlite::types::Type::Text, error))?;
    let Some(budget) = NonZeroU64::new(request.budget) else {
        return Err(sql::conversion_error(
            3,
            rusqlite::types::Type::Text,
            "invalid rewrite budget",
        ));
    };
    let [
        source_bucket,
        source_object,
        destination_bucket,
        destination_object,
    ] = request.path;
    let [
        if_generation_match,
        if_generation_not_match,
        if_metageneration_match,
        if_metageneration_not_match,
    ] = request.source_conditions;
    let id = row.get(0)?;
    let token = row.get(1)?;
    let previous_token = row.get(2)?;
    let offset = row.get(4)?;
    let total = row.get(5)?;
    let deadline = row.get(6)?;
    let response = row.get::<_, Option<sql::Json<Value>>>(7)?;
    let transfer_id: Option<BlobId> = row.get(8)?;
    let invalid = |error| sql::conversion_error(3, rusqlite::types::Type::Text, error);
    let response = response
        .map(|response| StoredResponse::try_from(response.0))
        .transpose()
        .map_err(invalid)?;
    let session = RewriteSession {
        id,
        token,
        previous_token,
        binding: RewriteBinding {
            source: ObjectTarget::new(source_bucket, source_object, request.generation),
            destination: ObjectTarget::new(
                destination_bucket.clone(),
                destination_object.clone(),
                (),
            ),
            parameters: request.parameters,
            metadata: request.body,
        },
        destination: DerivedDestination {
            component_count: request.components,
            bucket: destination_bucket,
            name: destination_object,
            content_type: options.content_type,
            metadata: options.metadata,
            storage_class: request.class,
            declared: Default::default(),
            recorded: options.expected,
            conditions: options.conditions,
            access: BucketAccess::Permissive,
        },
        source_conditions: ObjectConditions {
            if_generation_match,
            if_generation_not_match,
            if_metageneration_match,
            if_metageneration_not_match,
        },
        source_metageneration: request.metageneration,
        source_incarnation: request.source_incarnation,
        destination_incarnation: request.destination_incarnation,
        source_key_sha256: request.source_key_sha256,
        budget,
        offset,
        total,
        deadline,
        response,
    };
    if !session_consistent(&session, transfer_id) {
        return Err(invalid(InvalidRewriteSession));
    }
    Ok(session)
}

/// Whether a decoded session's identifiers, progress and stored response agree; an
/// incomplete session owns the derived transfer with its own ID, a completed one none.
fn session_consistent(session: &RewriteSession, transfer_id: Option<BlobId>) -> bool {
    let progress_valid = match &session.response {
        None => {
            session.offset == 0
                && session.previous_token.is_none()
                && transfer_id == Some(session.id)
        }
        Some(StoredResponse::Progress { offset, token }) => {
            *offset == session.offset
                && session.offset < session.total
                && *token == session.token
                && transfer_id == Some(session.id)
        }
        Some(StoredResponse::Complete { offset, .. }) => {
            *offset == session.offset && session.offset == session.total && transfer_id.is_none()
        }
    };
    progress_valid
        && session.binding.source.generation > 0
        && session.source_metageneration > 0
        && session.offset <= session.total
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    // Row by row: nothing bounds how many sessions a catalog holds, so neither may this check.
    let mut statement = connection.prepare_cached(concat!(
        "SELECT ",
        session_columns!(),
        " FROM rewrite_sessions ORDER BY id"
    ))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        read(row).map_err(super::unavailable)?;
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "open-time recovery probes and drops expired sessions once"
)]
pub(super) fn recover_due(
    connection: &mut Connection,
    now: UnixMillis,
) -> Result<(), CatalogError> {
    let due: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM rewrite_sessions WHERE deadline<=?1)",
        [now],
        |row| row.get(0),
    )?;
    if !due {
        return Ok(());
    }
    let transaction = connection.transaction()?;
    transaction.execute("INSERT OR IGNORE INTO pending_blob_deletions SELECT transfer_id FROM rewrite_sessions WHERE deadline<=?1 AND transfer_id IS NOT NULL",[now])?;
    transaction.execute("DELETE FROM derived_inputs WHERE transfer_id IN (SELECT id FROM rewrite_sessions WHERE deadline<=?1)",[now])?;
    transaction.execute("DELETE FROM derived_transfers WHERE id IN (SELECT id FROM rewrite_sessions WHERE deadline<=?1)",[now])?;
    transaction.execute("DELETE FROM rewrite_sessions WHERE deadline<=?1", [now])?;
    transaction.commit().map_err(super::unavailable)
}

/// The rewrite sessions that read from or write to `bucket`, in any incarnation of the name. Only
/// the stored request names the buckets, so this reads every session; a bucket purge is rare.
pub(super) fn bucket_sessions(
    connection: &Connection,
    bucket: &str,
) -> Result<Vec<BlobId>, CatalogError> {
    connection
        .prepare_cached("SELECT id FROM rewrite_sessions WHERE json_extract(request, '$.path[0]') = ?1 OR json_extract(request, '$.path[2]') = ?1")?
        .query_map([bucket], |row| row.get::<_, BlobId>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(CatalogError::from)
}

/// Drops the oldest finished rewrites past the shared window, keeping the one this request just
/// finished. A rewrite never fails over this bookkeeping, and an excess past one transaction's
/// batch carries over to the next completion.
///
/// shortcut: eviction follows creation deadlines; add completion ordering for recent-finish
/// retention.
fn trim_finished(
    connection: &sql::WriteTx<'_>,
    finished_now: &BlobId,
    budget: Option<i64>,
) -> Result<(), ObjectError> {
    // Only a budget needs the stored sizes, and adding them up reads every finished row. A
    // completion is rare next to a resumable creation, so this reads them instead of keeping
    // running totals the way resumable admission does.
    let (finished, finished_bytes) = match budget {
        None => (
            sql::scalar(
                connection,
                "SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NULL",
                [],
            )?,
            0,
        ),
        Some(_) => sql::row(
            connection,
            "SELECT count(*), coalesce(sum(2048 + length(CAST(request AS BLOB)) + length(CAST(response AS BLOB))), 0) FROM rewrite_sessions WHERE transfer_id IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?,
    };
    let excess = finished_excess(finished, finished_bytes, budget).min(MAX_TRIMMED_AT_ONCE);
    if excess > 0 {
        sql::exec(
            connection,
            "DELETE FROM rewrite_sessions WHERE id IN (SELECT id FROM rewrite_sessions WHERE transfer_id IS NULL AND id!=?1 ORDER BY deadline,id LIMIT ?2)",
            params![finished_now, excess],
        )?;
    }
    Ok(())
}

fn check_source(
    connection: &Connection,
    session: &RewriteSession,
    access: &BucketAccess,
) -> Result<ObjectRecord, ObjectError> {
    check_incarnations(connection, session)?;
    check_source_generation(connection, session, access)
}

fn check_source_generation(
    connection: &Connection,
    session: &RewriteSession,
    access: &BucketAccess,
) -> Result<ObjectRecord, ObjectError> {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every other object error passes through unchanged"
    )]
    let source = Catalog::check_derived_source_in_transaction(
        connection,
        &session.binding.source.bucket,
        &session.binding.source.name,
        Some(session.binding.source.generation),
        session.source_conditions,
        access,
    )
    .map_err(|error| match error {
        ObjectError::ObjectNotFound | ObjectError::BucketNotFound => {
            ObjectError::PreconditionFailed
        }
        other => other,
    })?;
    let live_changed = !session.binding.parameters.contains_key("sourceGeneration")
        && super::objects::lookup(
            connection,
            &session.binding.source.bucket,
            &session.binding.source.name,
        )?
        .is_none_or(|stored| stored.record.generation != session.binding.source.generation);
    if live_changed
        || source.generation != session.binding.source.generation
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
    if incarnation(connection, &session.binding.source.bucket)? != session.source_incarnation
        || incarnation(connection, &session.binding.destination.bucket)?
            != session.destination_incarnation
    {
        return Err(ObjectError::PreconditionFailed);
    }
    Ok(())
}

fn progress(session: &RewriteSession, response: &StoredResponse) -> RewriteProgress {
    let (token, resource) = match response {
        StoredResponse::Progress { token, .. } => (Some(token.to_string()), None),
        StoredResponse::Complete { resource, .. } => (None, Some(resource.clone())),
    };
    RewriteProgress {
        total: session.total,
        offset: response.offset(),
        token,
        resource,
        fields: session
            .binding
            .parameters
            .get("fields")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

impl Catalog {
    /// A missing source has a protocol-specific diagnostic. Destination authorization precedes it.
    pub(crate) async fn rewrite_buckets(
        &self,
        source: &str,
        destination: &ObjectTarget<()>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<Option<RewriteBuckets>, ObjectError> {
        let source = source.to_owned();
        let destination = destination.clone();
        let access = access.clone();
        let clock = self.0.policy.clock.clone();
        self.run_read(move |connection| {
            let snapshot = sql::read_snapshot(connection)?;
            super::objects::check_upload_destination(
                &snapshot,
                &destination.bucket,
                &destination.name,
                conditions,
                &access,
                clock.now(),
            )?;
            let Some(from) = super::buckets::select_bucket(&snapshot, &source)? else {
                return Ok(None);
            };
            let to = super::buckets::select_bucket(&snapshot, &destination.bucket)?
                .ok_or(ObjectError::BucketNotFound)?;
            Ok(Some(RewriteBuckets {
                access: access.clone(),
                conditions,
                source: incarnation(&snapshot, &source)?,
                destination: incarnation(&snapshot, &destination.bucket)?,
                same_location: from.location == to.location,
                destination_class: to.storage_class,
            }))
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn begin_rewrite_session(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
        start: RewriteStart,
    ) -> Result<(RewriteToken, super::derived::DerivedRequestGuard), RewriteError> {
        let buckets = self
            .rewrite_buckets(
                &start.binding.source.bucket,
                &start.binding.destination,
                start.destination.conditions,
                &start.destination.access,
            )
            .await?
            .ok_or(ObjectError::BucketNotFound)?;
        self.begin_checked_rewrite(segments, max_bytes, start, buckets)
            .await
            .map(|(step, guard)| (step.token, guard))
    }

    pub(crate) async fn begin_checked_rewrite(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
        start: RewriteStart,
        buckets: RewriteBuckets,
    ) -> Result<(RewriteStep, super::derived::DerivedRequestGuard), RewriteError> {
        let token = RewriteToken::generate().map_err(object_unavailable)?;
        let pin = match segments.as_slice() {
            [segment]
                if segment.offset == 0
                    && segment.length == segment.source.object.size
                    && start.budget == UNLIMITED_REWRITE_BUDGET
                    && buckets.keeps_placement(
                        segment.source.object.storage_class,
                        start.destination.storage_class,
                    ) =>
            {
                segment.source.publication_pin().await?
            }
            _ => None,
        };
        // shortcut: crash before delivery leaves durable state until expiry; add a delivered marker for eager recovery.
        let catalog = self.clone();
        let (transfer, owner) = self
            .begin_derived_with_binding(
                segments,
                max_bytes,
                super::derived::TransferKind::Continuable,
                move |connection, transfer| {
                    if let Some(limit) = catalog.session_limits().active_rewrite {
                        let count: i64 = sql::scalar(
                            connection,
                            "SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NOT NULL",
                            [],
                        )?;
                        if count >= i64::from(limit.get()) {
                            return Err(object_unavailable(BackendFailure::RewriteSessionLimit));
                        }
                    }
                    // Cloud Storage refused a class the destination cannot hold on the first
                    // call, before any byte was copied (recorded 2026-11-17).
                    super::objects::BucketPublication::load(connection, &start.destination.bucket)?
                        .check_class(start.destination.storage_class)?;
                    super::objects::check_upload_destination(
                        connection,
                        &start.destination.bucket,
                        &start.destination.name,
                        buckets.conditions,
                        &buckets.access,
                        catalog.0.policy.clock.now(),
                    )?;
                    let request = encode(&start, buckets.source, buckets.destination)?;
                    let deadline = UnixMillis::try_from(transfer.deadline)?;
                    sql::exec(
                        connection,
                        "INSERT INTO rewrite_sessions VALUES (?1,?2,NULL,?3,0,?4,?5,NULL,?1)",
                        params![transfer.id, token, request, transfer.total, deadline],
                    )?;
                    let session = sql::row(
                        connection,
                        concat!(
                            "SELECT ",
                            session_columns!(),
                            " FROM rewrite_sessions WHERE id=?1"
                        ),
                        [&transfer.id],
                        read,
                    )?;
                    check_source(connection, &session, &start.source_access)?;
                    Ok(catalog.guard_derived_request(&transfer.id))
                },
            )
            .await?;
        self.0.maintenance.wake.notify_one();
        Ok((
            RewriteStep {
                id: transfer.id,
                token,
                pin,
            },
            owner,
        ))
    }

    /// The session a client names by its `rewriteToken`; text that is not a token is invalid.
    #[cfg(test)]
    pub(crate) async fn rewrite_session(
        &self,
        token: &str,
    ) -> Result<RewriteSession, RewriteError> {
        self.rewrite_step(token).await.map(|(_, session)| session)
    }

    pub(crate) async fn rewrite_step(
        &self,
        token: &str,
    ) -> Result<(RewriteStep, RewriteSession), RewriteError> {
        let token = token.parse().map_err(|_| RewriteError::Invalid)?;
        let session = self.session_by_token(token).await?;
        Ok((
            RewriteStep {
                id: session.id,
                token,
                pin: None,
            },
            session,
        ))
    }

    async fn session_by_token(&self, token: RewriteToken) -> Result<RewriteSession, RewriteError> {
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        self.run_read(move |connection| read_session(connection, &token, now))
            .await?
            .ok_or(RewriteError::Gone)
    }

    #[cfg(test)]
    pub(crate) async fn continue_rewrite(
        &self,
        token: &str,
        source_access: BucketAccess,
        destination_access: BucketAccess,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<RewriteProgress, RewriteError> {
        let (step, _) = self.rewrite_step(token).await?;
        self.advance_rewrite(step, source_access, destination_access, render)
            .await
    }

    pub(crate) async fn advance_rewrite(
        &self,
        step: RewriteStep,
        source_access: BucketAccess,
        destination_access: BucketAccess,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<RewriteProgress, RewriteError> {
        let RewriteStep { id, token, pin } = step;
        let _guard = self
            .wait_for_writer(WriterKey::Rewrite(id), KeyOrigin::Holder)
            .await
            .map_err(CatalogError::from)?;
        // A request cancelled mid-step leaves that step running detached, holding the transfer. It
        // may yet finish the rewrite, so wait it out before reading the state this call acts on.
        // This writer keeps any later step out, so the claim need not outlive the wait.
        drop(
            self.wait_for_writer_under_way(WriterKey::Derived(id), KeyOrigin::Holder)
                .await
                .map_err(CatalogError::from)?,
        );
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        let source_check = source_access.clone();
        let destination_check = destination_access.clone();
        let (mut session, source_result) = self
            .run(move |connection| {
                let transaction = connection.transaction().map_err(object_unavailable)?;
                let checked = read_session(&transaction, &token, now)?
                    .filter(|session| session.id == id)
                    .ok_or(RewriteError::Gone)?;
                let result = check_continuation(
                    &transaction,
                    &checked,
                    &token,
                    (&source_check, &destination_check),
                    now,
                );
                Ok::<_, RewriteError>((checked, result))
            })
            .await?;
        let replay = session.previous_token == Some(token);
        if let Err(error) = source_result {
            if !matches!(session.response, Some(StoredResponse::Complete { .. })) {
                self.cancel_terminal_rewrite(&id, &error).await;
            }
            return Err(error.into());
        }
        if replay {
            return Ok(progress(
                &session,
                session
                    .response
                    .as_ref()
                    .ok_or_else(|| object_unavailable("missing rewrite replay"))?,
            ));
        }
        let current =
            source_result?.ok_or_else(|| object_unavailable("missing rewrite transfer"))?;
        let target = session
            .offset
            .saturating_add(session.budget.get())
            .min(session.total);
        let current = if pin.is_some() {
            current
        } else {
            self.advance_derived(&session.id, target.saturating_sub(current.offset))
                .await?
        };
        if pin.is_some() || current.offset == current.total {
            session.destination.access = destination_access;
            let destination = session.destination.clone();
            let clock = self.0.policy.clock.clone();
            let validate_session = session.clone();
            let completed_session = session.clone();
            let finished_budget = self.session_limits().finished_session_bytes;
            // shortcut: publish-time permission failures discard progress; specialize cleanup when retry must survive revocation.
            let (_, response) = self.publish_derived_prepared(current.publication_body(pin),destination,move |connection,record| {
                if validate_session.deadline<=UnixMillis::try_from(clock.now())? {return Err(ObjectError::DerivedTransferExpired);}
                let source = check_source(connection,&validate_session,&source_access)?;
                let same_location: bool = sql::scalar(connection, "SELECT source.location=destination.location FROM buckets source JOIN buckets destination ON destination.name=?2 WHERE source.name=?1", params![source.bucket,record.bucket])?;
                let location = if same_location { crate::json::DestinationLocation::Same } else { crate::json::DestinationLocation::Different };
                record.component_count = crate::json::derived_component_count(&source,record.storage_class,location);
                Ok(())
            },move |connection,record| {
                let response=StoredResponse::Complete { offset: record.size, resource: render(record)? };
                let saved=serde_json::to_string(&response).map_err(object_unavailable)?;
                let changed = sql::exec(connection, "UPDATE rewrite_sessions SET previous_token = token, offset = total, response = ?2, transfer_id = NULL WHERE id = ?1 AND token = ?3 AND transfer_id = id", params![session.id,saved,token])?;
                if changed != 1 { return Err(ObjectError::DerivedTransferNotFound); }
                trim_finished(connection, &session.id, finished_budget)?;
                // The stored response stays authoritative if the caller has gone by now.
                Ok(response)
            },None).await?;
            return Ok(progress(&completed_session, &response));
        }
        let next = RewriteToken::generate().map_err(object_unavailable)?;
        let response = StoredResponse::Progress {
            offset: current.offset,
            token: next,
        };
        let saved = serde_json::to_string(&response).map_err(object_unavailable)?;
        let progress = progress(&session, &response);
        let clock = self.0.policy.clock.clone();
        let checkpoint = self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            let now = clock.now();
            let changed = sql::exec(&transaction, "UPDATE rewrite_sessions SET previous_token=token, token=?2, offset=?3, response=?4 WHERE id=?1 AND deadline>?5 AND token=?6 AND transfer_id=id", params![id,next,current.offset,saved,UnixMillis::try_from(now)?,token])?;
            if changed != 1 {
                return Err(ObjectError::DerivedTransferNotFound);
            }
            // Mid-copy source changes invalidate the same session as the guarded pre-check.
            if incarnation(&transaction, &session.binding.source.bucket)? != session.source_incarnation {
                return Err(ObjectError::PreconditionFailed);
            }
            check_source_generation(&transaction, &session, &source_access)?;
            transaction.commit()?;
            Ok(())
        }).await;
        if let Err(error) = checkpoint {
            self.cancel_terminal_rewrite(&id, &error).await;
            return Err(error.into());
        }
        Ok(progress)
    }

    async fn cancel_terminal_rewrite(&self, id: &BlobId, error: &ObjectError) {
        if matches!(
            error,
            ObjectError::PreconditionFailed
                | ObjectError::BucketNotFound
                | ObjectError::ObjectNotFound
        ) && let Err(cleanup) = self.cancel_derived(id).await
        {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::RewriteCleanupDeferred,
                serde_json::json!({"message":format!("terminal rewrite cleanup failed: {cleanup}")}),
            );
        }
    }

    pub(super) async fn maintain_rewrite_sessions(
        &self,
        now: UnixMillis,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let mut sweep = SweepOutcome::default();
        let mut after = super::After::Start;
        loop {
            let cursor = after.clone();
            let ids=self.run(move |connection| connection.prepare_cached("SELECT id FROM rewrite_sessions WHERE deadline<=?1 AND id>?2 ORDER BY id LIMIT 16")?.query_map(params![now,cursor],|row|row.get::<_,BlobId>(0))?.collect::<rusqlite::Result<Vec<_>>>().map_err(super::unavailable)).await?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                after = super::After::Key(id);
                let _guard = match self.claim_sweep_writer(WriterKey::Rewrite(id)).await {
                    Ok(guard) => guard,
                    Err(ClaimError::Busy) => continue,
                    Err(error) => {
                        sweep.fail(REWRITE_SWEEP, CatalogError::from(error));
                        continue;
                    }
                };
                match self.expire_derived(&id).await {
                    Ok(()) | Err(ObjectError::DerivedTransferNotFound) => {}
                    Err(ObjectError::DerivedTransferBusy) => continue,
                    Err(error) => {
                        sweep.fail(REWRITE_SWEEP, super::unavailable(error));
                        continue;
                    }
                }
                if let Err(error) = self
                    .run(move |connection| {
                        sql::exec_on_writer(
                            connection,
                            "DELETE FROM rewrite_sessions WHERE id=?1 AND deadline<=?2",
                            params![id, now],
                        )
                        .map(|_| ())
                        .map_err(super::unavailable)
                    })
                    .await
                {
                    sweep.fail(REWRITE_SWEEP, error);
                }
            }
        }
        let next = self.next_rewrite_deadline(now).await;
        sweep.record(REWRITE_SWEEP, next);
        sweep.finish()
    }

    async fn next_rewrite_deadline(
        &self,
        now: UnixMillis,
    ) -> Result<Option<SystemTime>, CatalogError> {
        self.run(move |connection| {
            let deadline: Option<UnixMillis> = sql::scalar(
                connection,
                "SELECT min(deadline) FROM rewrite_sessions WHERE deadline>?1",
                [now],
            )?;
            deadline
                .map(UnixMillis::system_time)
                .transpose()
                .map_err(super::unavailable)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::super::session_limits::MAX_FINISHED_SESSIONS;
    use super::*;
    use crate::clock::ManualClock;
    use crate::storage::test_fixtures::{PERMISSIVE, body, bucket, live, permissive_upload};
    use serde_json::json;
    use std::{
        sync::Arc,
        time::{Duration, UNIX_EPOCH},
    };

    #[test]
    fn persisted_request_keeps_its_json_bytes() {
        let start = RewriteStart {
            binding: RewriteBinding {
                source: crate::object::ObjectTarget::new("bucket", "source", 7),
                destination: crate::object::ObjectTarget::new("bucket", "destination", ()),
                parameters: Default::default(),
                metadata: None,
            },
            destination: DerivedDestination {
                bucket: "bucket".into(),
                name: "destination".into(),
                content_type: "text/plain".into(),
                ..DerivedDestination::new(BucketAccess::Permissive)
            },
            source_conditions: ObjectConditions::default(),
            source_metageneration: 1,
            source_access: BucketAccess::Permissive,
            source_key_sha256: None,
            budget: std::num::NonZeroU64::new(1_048_576).unwrap(),
        };
        let encoded = encode(
            &start,
            "00000000000000000000000000000001".parse().unwrap(),
            "00000000000000000000000000000002".parse().unwrap(),
        )
        .unwrap();
        let expected = concat!(
            r#"{"path":["bucket","source","bucket","destination"],"generation":7,"metageneration":1,"parameters":{},"body":null,"source_incarnation":"00000000000000000000000000000001","destination_incarnation":"00000000000000000000000000000002","options":"#,
            r#"{"version":2,"content_type":"text/plain","metadata":{"replace":false,"eventBasedHold":null,"temporaryHold":null,"customTime":null,"contentType":null,"contentEncoding":null,"contentDisposition":null,"contentLanguage":null,"cacheControl":null,"metadata":null,"retention":null,"customerKeySha256":null},"md5":null,"crc32c":null,"conditions":[null,null,null,null],"access":null,"origin":null,"full":false,"response_fields":null}"#,
            r#","class":null,"components":null,"source_conditions":[null,null,null,null],"budget":1048576,"source_key_sha256":null}"#
        );
        assert_eq!(encoded, expected);
        let connection = Connection::open_in_memory().unwrap();
        let session = connection
            .query_row(
                "SELECT ?1,?2,NULL,?3,0,8,604801000,NULL,?1",
                params!["00000000000000000000000000000003", "0".repeat(64), &encoded],
                read,
            )
            .unwrap();
        assert_eq!(session.binding, start.binding);
        assert_eq!(
            session.destination.content_type,
            start.destination.content_type
        );
        assert_eq!(session.budget, start.budget);
        for (offset, total, response, column) in
            [(-1, 8, None, 4), (0, -1, None, 5), (0, 8, Some("{"), 7)]
        {
            let error = connection
                .query_row(
                    "SELECT ?1,?2,NULL,?3,?4,?5,604801000,?6,?1",
                    params![
                        "00000000000000000000000000000003",
                        "0".repeat(64),
                        &encoded,
                        offset,
                        total,
                        response
                    ],
                    read,
                )
                .err()
                .unwrap();
            if column == 7 {
                assert!(matches!(
                    error,
                    rusqlite::Error::FromSqlConversionFailure(7, _, _)
                ));
                assert!(std::error::Error::source(&error).is_some());
            } else {
                assert!(
                    matches!(error, rusqlite::Error::IntegralValueOutOfRange(index, -1) if index == column)
                );
            }
        }
    }
    async fn fixture() -> (Catalog, std::path::PathBuf, Arc<ManualClock>) {
        fixture_with_limits(None).await
    }

    async fn fixture_with_limits(
        limits: Option<&crate::config::Config>,
    ) -> (Catalog, std::path::PathBuf, Arc<ManualClock>) {
        let directory = std::env::temp_dir().join(format!(
            "gcs-mimic-rewrite-{}",
            RewriteToken::generate().unwrap()
        ));
        let clock = Arc::new(ManualClock::new(UNIX_EPOCH + Duration::from_millis(1000)));
        let catalog = Catalog::open_disk_with_limits(&directory, clock.clone(), limits)
            .await
            .unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        catalog
            .put_object(permissive_upload("bucket", "source", 8), body(vec![]))
            .await
            .unwrap();
        (catalog, directory, clock)
    }

    async fn start_request(catalog: &Catalog) -> (DerivedSegment, RewriteStart) {
        let source = catalog
            .resolve_derived_source(live("bucket", "source"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        let start = RewriteStart {
            binding: RewriteBinding {
                source: crate::object::ObjectTarget::new(
                    "bucket",
                    "source",
                    source.object.generation,
                ),
                destination: crate::object::ObjectTarget::new("bucket", "destination", ()),
                parameters: Default::default(),
                metadata: None,
            },
            destination: DerivedDestination {
                bucket: "bucket".into(),
                name: "destination".into(),
                content_type: "text/plain".into(),
                ..crate::storage::DerivedDestination::new(crate::bucket::BucketAccess::Permissive)
            },
            source_conditions: ObjectConditions::default(),
            source_metageneration: source.object.metageneration,
            source_access: BucketAccess::Permissive,
            source_key_sha256: None,
            budget: std::num::NonZeroU64::new(1_048_576).unwrap(),
        };
        (source.whole(), start)
    }

    async fn new_session(
        catalog: &Catalog,
    ) -> Result<(String, super::super::derived::DerivedRequestGuard), RewriteError> {
        let (segment, start) = start_request(catalog).await;
        catalog
            .begin_rewrite_session(vec![segment], 8, start)
            .await
            .map(|(token, guard)| (token.to_string(), guard))
    }

    #[tokio::test]
    async fn only_same_placement_one_call_rewrites_publish_the_pinned_body() {
        let (catalog, directory, _) = fixture().await;
        catalog
            .put_object(
                permissive_upload("bucket", "source", 8),
                body(vec![bytes::Bytes::from_static(b"bytes")]),
            )
            .await
            .unwrap();
        for class in [None, Some(crate::bucket::StorageClass::Nearline)] {
            let (segment, mut start) = start_request(&catalog).await;
            let buckets = catalog
                .rewrite_buckets(
                    "bucket",
                    &start.binding.destination,
                    Default::default(),
                    PERMISSIVE,
                )
                .await
                .unwrap()
                .unwrap();
            start.budget = buckets.budget(segment.source.object.storage_class, class, None);
            start.destination.storage_class = class;
            let (step, mut owner) = catalog
                .begin_checked_rewrite(vec![segment], 8, start, buckets)
                .await
                .unwrap();
            assert_eq!(step.pin.is_some(), class.is_none());
            let token = step.token.to_string();
            let result = catalog
                .advance_rewrite(
                    step,
                    BucketAccess::Permissive,
                    BucketAccess::Permissive,
                    |_| Ok(serde_json::json!({"size":"5"})),
                )
                .await
                .unwrap();
            owner.disarm();
            assert_eq!(result.offset, 5);
            assert_eq!(result.token, None);
            let replay = continue_to_end(&catalog, &token).await.unwrap();
            assert_eq!(replay.resource, result.resource);
        }
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    async fn grant_create(catalog: &Catalog, name: &str) -> BucketAccess {
        let mut policy = catalog.get_policy(name, PERMISSIVE).await.unwrap();
        policy.bindings.push(crate::auth::Binding {
            role: "roles/storage.objectCreator".parse().unwrap(),
            members: vec!["allUsers".parse().unwrap()],
        });
        catalog.set_policy(name, policy, PERMISSIVE).await.unwrap();
        BucketAccess::Policy {
            identity: None,
            permission: crate::auth::OBJECTS_CREATE,
            project: Some("project".into()),
        }
    }

    #[tokio::test]
    async fn rewrite_bucket_snapshot_sees_placement_and_checks_destination_access_on_every_route() {
        let (catalog, directory, _) = fixture().await;
        let mut to = bucket("other", "project");
        to.storage_class = crate::bucket::StorageClass::Nearline;
        catalog.insert_bucket(to).await.unwrap();
        let destination = ObjectTarget::new("other", "destination", ());
        let access = grant_create(&catalog, "other").await;
        super::super::read_pool::tests::assert_point_read(&catalog, async || {
            let buckets = catalog
                .rewrite_buckets("bucket", &destination, Default::default(), &access)
                .await
                .unwrap()
                .unwrap();
            assert_ne!(buckets.source, buckets.destination);
            assert_eq!(
                buckets.budget(crate::bucket::StorageClass::Standard, None, None),
                DEFAULT_REWRITE_BUDGET
            );
        })
        .await;
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn registration_rechecks_a_revoked_grant_and_a_recreated_destination() {
        let (catalog, directory, _) = fixture().await;
        for recreate in [false, true] {
            let name = if recreate { "recreated" } else { "revoked" };
            catalog
                .insert_bucket(bucket(name, "project"))
                .await
                .unwrap();
            let (segment, mut start) = start_request(&catalog).await;
            start.binding.destination.bucket = name.into();
            start.destination.bucket = name.into();
            if !recreate {
                start.destination.access = grant_create(&catalog, name).await;
            }
            let buckets = catalog
                .rewrite_buckets(
                    "bucket",
                    &start.binding.destination,
                    Default::default(),
                    &start.destination.access,
                )
                .await
                .unwrap()
                .unwrap();
            if recreate {
                catalog
                    .delete_bucket(name, Default::default(), PERMISSIVE)
                    .await
                    .unwrap();
                catalog
                    .insert_bucket(bucket(name, "project"))
                    .await
                    .unwrap();
            } else {
                let mut revoked = catalog.get_policy(name, PERMISSIVE).await.unwrap();
                revoked.bindings.clear();
                catalog.set_policy(name, revoked, PERMISSIVE).await.unwrap();
            }
            let result = catalog
                .begin_checked_rewrite(vec![segment], 8, start, buckets)
                .await;
            if recreate {
                assert!(matches!(
                    result,
                    Err(RewriteError::Object(ObjectError::PreconditionFailed))
                ));
            } else {
                assert!(matches!(
                    result,
                    Err(RewriteError::Object(ObjectError::Denied(_)))
                ));
            }
            let sessions = catalog
                .run::<_, CatalogError>(|db| {
                    Ok(sql::scalar::<i64>(
                        db,
                        "SELECT count(*) FROM rewrite_sessions",
                        [],
                    )?)
                })
                .await
                .unwrap();
            assert_eq!(sessions, 0);
        }
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn a_source_changed_during_copy_cannot_checkpoint_or_publish() {
        for budget in [NonZeroU64::MIN, UNLIMITED_REWRITE_BUDGET] {
            let (catalog, directory, _) = fixture().await;
            catalog
                .put_object(
                    permissive_upload("bucket", "source", 8),
                    body(vec![bytes::Bytes::from_static(b"abc")]),
                )
                .await
                .unwrap();
            let (segment, mut start) = start_request(&catalog).await;
            start.budget = budget;
            let (token, mut owner) = catalog
                .begin_rewrite_session(vec![segment], 8, start)
                .await
                .unwrap();
            owner.disarm();
            catalog.run::<_, CatalogError>(|db| {
                sql::exec_on_writer(db, "CREATE TEMP TRIGGER change_source AFTER UPDATE OF offset ON derived_transfers WHEN NEW.offset > OLD.offset BEGIN UPDATE objects SET metageneration=metageneration+1 WHERE name='source'; END", [])?;
                Ok(())
            }).await.unwrap();
            let token = token.to_string();
            assert!(matches!(
                continue_to_end(&catalog, &token).await,
                Err(RewriteError::Object(ObjectError::PreconditionFailed))
            ));
            if budget == NonZeroU64::MIN {
                assert!(matches!(
                    continue_to_end(&catalog, &token).await,
                    Err(RewriteError::Gone)
                ));
            }
            assert!(matches!(
                catalog
                    .get_object(
                        live("bucket", "destination"),
                        Default::default(),
                        PERMISSIVE
                    )
                    .await,
                Err(ObjectError::ObjectNotFound)
            ));
            catalog.close().await.unwrap();
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn an_invalidated_lookup_cannot_advance_another_step() {
        let (catalog, directory, _) = fixture().await;
        catalog
            .put_object(
                permissive_upload("bucket", "source", 8),
                body(vec![bytes::Bytes::from_static(b"three")]),
            )
            .await
            .unwrap();
        let (segment, mut start) = start_request(&catalog).await;
        start.budget = NonZeroU64::MIN;
        let (token, mut owner) = catalog
            .begin_rewrite_session(vec![segment], 8, start)
            .await
            .unwrap();
        owner.disarm();
        let token = token.to_string();
        let (stale, _) = catalog.rewrite_step(&token).await.unwrap();
        let next = continue_to_end(&catalog, &token)
            .await
            .unwrap()
            .token
            .unwrap();
        let latest = continue_to_end(&catalog, &next).await.unwrap();
        assert!(matches!(
            catalog
                .advance_rewrite(
                    stale,
                    BucketAccess::Permissive,
                    BucketAccess::Permissive,
                    |_| panic!("stale token published")
                )
                .await,
            Err(RewriteError::Gone)
        ));
        assert_eq!(
            catalog
                .rewrite_session(&latest.token.unwrap())
                .await
                .unwrap()
                .offset,
            2
        );
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// The token a new session returns is the stored text, and only that spelling selects it.
    #[tokio::test]
    async fn issued_rewrite_tokens_are_the_stored_text_and_uppercase_is_invalid() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        catalog
            .put_object(
                permissive_upload("bucket", "source", 8),
                body(vec![bytes::Bytes::from_static(b"contents")]),
            )
            .await
            .unwrap();
        let (token, mut owner) = new_session(&catalog).await.unwrap();
        owner.disarm();
        let stored = catalog
            .run::<_, CatalogError>(|connection| {
                Ok(sql::scalar::<String>(
                    connection,
                    "SELECT token FROM rewrite_sessions",
                    [],
                )?)
            })
            .await
            .unwrap();
        assert_eq!(token, stored);
        super::super::read_pool::tests::assert_point_read(&catalog, async || {
            assert_eq!(
                catalog
                    .rewrite_session(&token)
                    .await
                    .unwrap()
                    .token
                    .to_string(),
                stored
            );
        })
        .await;
        assert!(matches!(
            catalog.rewrite_session(&token.to_ascii_uppercase()).await,
            Err(RewriteError::Invalid)
        ));
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn expiry_during_copy_returns_gone_when_checkpointing_the_next_token() {
        let dir = crate::test_support::TestDir::new();
        let clock = Arc::new(ManualClock::new(UNIX_EPOCH + Duration::from_millis(1000)));
        let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
            .await
            .unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        catalog
            .put_object(
                permissive_upload("bucket", "source", 8),
                body(vec![bytes::Bytes::from_static(b"contents")]),
            )
            .await
            .unwrap();
        let (token, mut owner) = new_session(&catalog).await.unwrap();
        owner.disarm();
        catalog
            .run::<_, CatalogError>(|connection| {
                sql::exec_on_writer(
                    connection,
                    "UPDATE rewrite_sessions SET request=json_set(request,'$.budget',1)",
                    [],
                )?;
                sql::exec_on_writer(connection, "CREATE TRIGGER expire_after_progress AFTER UPDATE OF offset ON derived_transfers WHEN NEW.offset > OLD.offset BEGIN UPDATE rewrite_sessions SET deadline=0 WHERE id=NEW.id; END", [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            catalog
                .continue_rewrite(
                    &token,
                    BucketAccess::Permissive,
                    BucketAccess::Permissive,
                    |_| Ok(json!({}))
                )
                .await,
            Err(RewriteError::Gone)
        ));
        let original = token.clone();
        catalog.run::<_, CatalogError>(move |connection| {
            let (token, deadline, offset): (String, i64, u64) = sql::row(connection, "SELECT r.token, r.deadline, d.offset FROM rewrite_sessions r JOIN derived_transfers d ON d.id=r.id", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            assert_eq!(token, original);
            assert_eq!(deadline, 0);
            assert_eq!(offset, 1);
            Ok(())
        }).await.unwrap();
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn unfinished_rewrites_past_the_former_cap_survive_reopening_and_continue() {
        let (catalog, directory, clock) = fixture().await;
        let mut tokens = Vec::new();
        for _ in 0..300 {
            let (token, mut owner) = new_session(&catalog).await.unwrap();
            owner.disarm();
            tokens.push(token);
        }
        catalog.close().await.unwrap();
        let catalog = Catalog::open_with_clock(&directory, clock).await.unwrap();
        for token in &tokens {
            catalog.rewrite_session(token).await.unwrap();
        }
        let progress = catalog
            .continue_rewrite(
                &tokens[299],
                BucketAccess::Permissive,
                BucketAccess::Permissive,
                |record| Ok(json!({"generation":record.generation})),
            )
            .await
            .unwrap();
        assert!(progress.resource.is_some());
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn a_configured_rewrite_cap_refuses_past_it_and_busy_expiry_does_not_block_others() {
        let config = crate::config::ConfigDraft {
            max_rewrite_sessions: std::num::NonZeroU32::new(128),
            ..Default::default()
        }
        .build();
        let (catalog, directory, clock) = fixture_with_limits(Some(&config)).await;
        let mut first = None;
        for index in 0..=128 {
            let result = new_session(&catalog).await;
            if index == 128 {
                assert!(matches!(
                    result,
                    Err(RewriteError::Object(ObjectError::Unavailable(cause)))
                        if cause.failure() == Some(BackendFailure::RewriteSessionLimit)
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
            .claim_writer(WriterKey::Rewrite(session.id), KeyOrigin::Holder)
            .unwrap();
        let derived_guard = catalog
            .claim_writer(WriterKey::Derived(session.id), KeyOrigin::Holder)
            .unwrap();
        clock.set(UNIX_EPOCH + Duration::from_millis(604801000));
        assert!(matches!(
            catalog.rewrite_session(&token).await,
            Err(RewriteError::Gone)
        ));
        assert!(
            catalog
                .maintain_rewrite_sessions(UnixMillis(604801000))
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
        catalog.0.maintenance.wake.notified().await;
        drop(guard);
        tokio::time::timeout(
            Duration::from_millis(100),
            catalog.0.maintenance.wake.notified(),
        )
        .await
        .unwrap();
        assert!(
            catalog
                .maintain_rewrite_sessions(UnixMillis(604801000))
                .await
                .unwrap()
                .is_none()
        );
        let remaining: i64 = catalog
            .run(|connection| {
                sql::scalar(connection, "SELECT count(*) FROM rewrite_sessions", [])
                    .map_err(super::super::unavailable)
            })
            .await
            .unwrap();
        assert_eq!(remaining, 1);
        drop(derived_guard);
        assert!(
            catalog
                .maintain_rewrite_sessions(UnixMillis(604801000))
                .await
                .unwrap()
                .is_none()
        );
        let source = catalog
            .get_object(live("bucket", "source"), Default::default(), PERMISSIVE)
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
    async fn rewrite_prechecks_use_the_expected_catalog_jobs() {
        let (catalog, directory, _) = fixture().await;
        let (writes, reads) = catalog.job_counts();
        assert!(
            catalog
                .has_bucket_permission("bucket", PERMISSIVE)
                .await
                .unwrap()
        );
        let source = catalog
            .resolve_derived_source(live("bucket", "source"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        catalog
            .rewrite_buckets(
                "bucket",
                &ObjectTarget::new("bucket", "destination", ()),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(catalog.job_counts(), (writes + 1, reads + 2));
        drop(source);

        let token = finish_rewrite(&catalog).await;
        let (writes, reads) = catalog.job_counts();
        let (step, session) = catalog.rewrite_step(&token).await.unwrap();
        assert_eq!(session.binding.destination.name, "destination");
        assert!(
            catalog
                .advance_rewrite(
                    step,
                    BucketAccess::Permissive,
                    BucketAccess::Permissive,
                    |_| panic!("a replay uses its stored response")
                )
                .await
                .unwrap()
                .resource
                .is_some()
        );
        assert_eq!(catalog.job_counts(), (writes + 1, reads + 1));
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    async fn finish_rewrite(catalog: &Catalog) -> String {
        let (token, mut owner) = new_session(catalog).await.unwrap();
        owner.disarm();
        let finished = continue_to_end(catalog, &token).await.unwrap();
        assert!(finished.resource.is_some());
        token
    }

    async fn continue_to_end(
        catalog: &Catalog,
        token: &str,
    ) -> Result<RewriteProgress, RewriteError> {
        catalog
            .continue_rewrite(
                token,
                BucketAccess::Permissive,
                BucketAccess::Permissive,
                |record| Ok(json!({"generation":record.generation})),
            )
            .await
    }

    async fn finished_count(catalog: &Catalog) -> i64 {
        catalog
            .run(|connection| {
                sql::scalar(
                    connection,
                    "SELECT count(*) FROM rewrite_sessions WHERE transfer_id IS NULL",
                    [],
                )
                .map_err(super::super::unavailable)
            })
            .await
            .unwrap()
    }

    /// Copies a real finished row, so reopening still validates every record. The small IDs sort
    /// before any generated one at the same deadline, making these the oldest in that order.
    async fn fill_finished_rewrites(catalog: &Catalog, count: i64) {
        catalog
            .run::<_, CatalogError>(move |connection| {
                sql::exec_on_writer(
                    connection,
                    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<?1) INSERT INTO rewrite_sessions SELECT printf('%032x',i), printf('%064x',i), printf('%064x',i), r.request, r.offset, r.total, r.deadline, r.response, NULL FROM n, (SELECT * FROM rewrite_sessions WHERE transfer_id IS NULL LIMIT 1) r",
                    [count],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    fn filled_token(index: i64) -> String {
        format!("{index:064x}")
    }

    #[tokio::test]
    async fn finished_rewrites_past_the_former_window_still_replay() {
        let (catalog, directory, clock) = fixture().await;
        let first = finish_rewrite(&catalog).await;
        for _ in 0..128 {
            finish_rewrite(&catalog).await;
        }
        assert_eq!(finished_count(&catalog).await, 129);
        assert!(
            continue_to_end(&catalog, &first)
                .await
                .unwrap()
                .resource
                .is_some()
        );
        // The longer window must also pass the checks of a later open.
        catalog.close().await.unwrap();
        let catalog = Catalog::open_with_clock(&directory, clock).await.unwrap();
        assert_eq!(finished_count(&catalog).await, 129);
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn the_oldest_finished_rewrite_past_the_window_goes_first() {
        let (catalog, directory, clock) = fixture().await;
        let real = finish_rewrite(&catalog).await;
        fill_finished_rewrites(&catalog, MAX_FINISHED_SESSIONS - 1).await;
        assert_eq!(finished_count(&catalog).await, MAX_FINISHED_SESSIONS);

        let newest = finish_rewrite(&catalog).await;

        assert_eq!(finished_count(&catalog).await, MAX_FINISHED_SESSIONS);
        assert!(matches!(
            catalog.rewrite_session(&filled_token(1)).await,
            Err(RewriteError::Gone)
        ));
        catalog.rewrite_session(&filled_token(2)).await.unwrap();
        catalog.rewrite_session(&real).await.unwrap();
        assert!(
            continue_to_end(&catalog, &newest)
                .await
                .unwrap()
                .resource
                .is_some()
        );
        catalog.close().await.unwrap();
        let catalog = Catalog::open_with_clock(&directory, clock).await.unwrap();
        assert_eq!(finished_count(&catalog).await, MAX_FINISHED_SESSIONS);
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn one_completion_trims_a_bounded_batch_and_later_ones_finish_the_rest() {
        let (catalog, directory, _clock) = fixture().await;
        finish_rewrite(&catalog).await;
        let batch = i64::try_from(MAX_TRIMMED_AT_ONCE).unwrap();
        fill_finished_rewrites(&catalog, MAX_FINISHED_SESSIONS + batch).await;
        finish_rewrite(&catalog).await;
        assert_eq!(finished_count(&catalog).await, MAX_FINISHED_SESSIONS + 2);
        finish_rewrite(&catalog).await;
        assert_eq!(finished_count(&catalog).await, MAX_FINISHED_SESSIONS);
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Finished rewrites must not outgrow a RAM catalog: past its budget every catalog write fails
    /// until the records expire days later. A response large enough that the former count-only
    /// window filled the catalog long before it trimmed anything shows the bytes decide.
    #[tokio::test]
    async fn finished_rewrites_stay_inside_a_small_memory_catalog_budget() {
        const BUDGET: u64 = 1024 * 1024;
        let config = crate::config::ConfigDraft {
            backend: crate::config::Backend::Memory,
            memory_catalog_max_bytes: BUDGET,
            ..Default::default()
        }
        .build();
        let catalog = Catalog::open_server_with_limits(
            crate::storage::CatalogBacking::Memory {
                blob_bytes: 64 * 1024 * 1024,
                catalog_bytes: BUDGET,
            },
            Arc::new(crate::clock::SystemClock),
            crate::storage::admin::LocalAdmin::Disabled,
            Some(&config),
        )
        .await
        .unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        catalog
            .put_object(permissive_upload("bucket", "source", 8), body(vec![]))
            .await
            .unwrap();
        let padding = "x".repeat(30_000);
        let mut newest = None;
        for _ in 0..300 {
            let (token, mut owner) = new_session(&catalog).await.unwrap();
            owner.disarm();
            let padding = padding.clone();
            catalog
                .continue_rewrite(
                    &token,
                    BucketAccess::Permissive,
                    BucketAccess::Permissive,
                    move |_| Ok(json!({ "padding": padding })),
                )
                .await
                .unwrap();
            newest = Some(token);
        }
        let kept = finished_count(&catalog).await;
        assert!((2..=10).contains(&kept), "{kept} records kept");
        catalog.rewrite_session(&newest.unwrap()).await.unwrap();
        catalog.close().await.unwrap();
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
                BucketAccess::Permissive,
                BucketAccess::Permissive,
                move |record| {
                    expiry_clock.set(UNIX_EPOCH + Duration::from_millis(604801000));
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
                .get_object(
                    live("bucket", "destination"),
                    Default::default(),
                    PERMISSIVE
                )
                .await
                .unwrap()
                .size,
            0
        );
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn a_retried_continuation_waits_for_the_request_still_running() {
        let config = crate::config::ConfigDraft {
            catalog_wait_timeout: crate::time_limit::TimeLimit::from_millis(60_000),
            ..Default::default()
        }
        .build();
        let (catalog, directory, _clock) = fixture_with_limits(Some(&config)).await;
        let (token, mut owner) = new_session(&catalog).await.unwrap();
        owner.disarm();
        let session = catalog.rewrite_session(&token).await.unwrap();
        let running = catalog
            .claim_writer(WriterKey::Rewrite(session.id), KeyOrigin::Holder)
            .unwrap();
        let worker = catalog.clone();
        let retry = tokio::spawn(async move {
            worker
                .continue_rewrite(
                    &token,
                    BucketAccess::Permissive,
                    BucketAccess::Permissive,
                    |_| Ok(json!({})),
                )
                .await
        });
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        assert!(!retry.is_finished());
        drop(running);
        retry.await.unwrap().unwrap();
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A long catalog wait, so that only the behaviour under test can end a wait.
    fn long_writer_wait() -> crate::config::Config {
        crate::config::ConfigDraft {
            catalog_wait_timeout: crate::time_limit::TimeLimit::from_millis(60_000),
            ..Default::default()
        }
        .build()
    }

    fn spawn_continuation(
        catalog: &Catalog,
        token: String,
    ) -> tokio::task::JoinHandle<Result<RewriteProgress, RewriteError>> {
        let worker = catalog.clone();
        tokio::spawn(async move {
            worker
                .continue_rewrite(
                    &token,
                    BucketAccess::Permissive,
                    BucketAccess::Permissive,
                    |record| Ok(json!({"generation":record.generation})),
                )
                .await
        })
    }

    /// Waits until a continuation holds its session, the step before it waits for a transfer.
    async fn claimed_its_session(catalog: &Catalog, session: BlobId) {
        crate::test_support::eventually_async("the continuation to claim its session", || async {
            matches!(
                catalog.claim_writer(WriterKey::Rewrite(session), KeyOrigin::Holder),
                Err(ClaimError::Busy)
            )
        })
        .await;
    }

    #[tokio::test]
    async fn a_retried_continuation_waits_for_the_step_its_cancelled_request_left_running() {
        let (catalog, directory, _clock) = fixture_with_limits(Some(&long_writer_wait())).await;
        let (token, mut owner) = new_session(&catalog).await.unwrap();
        owner.disarm();
        let session = catalog.rewrite_session(&token).await.unwrap();
        let detached = catalog
            .claim_writer(WriterKey::Derived(session.id), KeyOrigin::Holder)
            .unwrap();
        let mut retry = spawn_continuation(&catalog, token);
        claimed_its_session(&catalog, session.id).await;
        // Refusing takes one catalog job, far less than this bound on an idle machine, and a slow
        // machine can only make this check pass where it should not, never fail where it should.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut retry)
                .await
                .is_err()
        );
        drop(detached);
        assert!(retry.await.unwrap().unwrap().resource.is_some());
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_retry_during_a_detached_publication_returns_the_stored_result() {
        use crate::storage::catalog::fault::{Fault, Step};
        use std::sync::Mutex;
        let (catalog, directory, _clock) = fixture_with_limits(Some(&long_writer_wait())).await;
        let (token, mut owner) = new_session(&catalog).await.unwrap();
        owner.disarm();
        let session = catalog.rewrite_session(&token).await.unwrap();
        let (entered, reached) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        catalog.inject(
            Step::BeforePublish,
            0,
            Fault::Block {
                entered: Arc::new(Mutex::new(Some(entered))),
                release: Arc::new(Mutex::new(released)),
            },
        );
        let cancelled = spawn_continuation(&catalog, token.clone());
        reached.await.unwrap();
        cancelled.abort();
        assert!(cancelled.await.unwrap_err().is_cancelled());
        let retry = spawn_continuation(&catalog, token);
        claimed_its_session(&catalog, session.id).await;
        release.send(()).unwrap();
        // Reading the session before the publication ended would find its transfer gone.
        let progress = retry.await.unwrap().unwrap();
        assert!(progress.resource.is_some());
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn a_retried_continuation_finishes_ahead_of_a_job_draining_its_lease() {
        let (catalog, directory, _clock) = fixture_with_limits(Some(&long_writer_wait())).await;
        let (token, mut owner) = new_session(&catalog).await.unwrap();
        owner.disarm();
        let session = catalog.rewrite_session(&token).await.unwrap();
        let detached = catalog
            .claim_writer(WriterKey::Derived(session.id), KeyOrigin::Holder)
            .unwrap();
        let retry = spawn_continuation(&catalog, token);
        claimed_its_session(&catalog, session.id).await;
        let admin = catalog.admin(crate::storage::admin::Origin::Seed);
        let reset = tokio::spawn(async move {
            admin
                .reset_until(crate::time_limit::TimeLimit::from_millis(60_000).deadline())
                .await
                .map(drop)
        });
        crate::test_support::eventually_async("the reset to line up", || async {
            catalog.0.admin.reads.available_permits() == 31
        })
        .await;
        drop(detached);
        // Lining up behind the reset would wait for the lease the retry itself holds.
        assert!(
            crate::test_support::joined(retry)
                .await
                .unwrap()
                .resource
                .is_some()
        );
        crate::test_support::joined(reset).await.unwrap();
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn rewrite_initial_request_owner_reclaims_undelivered_state() {
        let (catalog, directory, _clock) = fixture().await;
        let (_token, owner) = new_session(&catalog).await.unwrap();
        drop(owner);
        crate::test_support::eventually_async(
            "the undelivered session to be reclaimed",
            || async {
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
                count == 0
                    && std::fs::read_dir(directory.join("staging"))
                        .unwrap()
                        .count()
                        == 0
            },
        )
        .await;
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
