//! Trusted local administration; never construct this authority from an API request.
//!
//! Administrative grants cannot be constructed by adapters:
//! ```compile_fail
//! use gcs_mimic::storage::admin::{AdminGrant, Origin};
//! let grant = AdminGrant { origin: Origin::Seed };
//! ```
//! Raw staging handles and SQL callback entry points stay inside the crate:
//! ```compile_fail
//! use gcs_mimic::storage::SessionLease;
//! ```
//! ```compile_fail
//! async fn bypass(catalog: &gcs_mimic::storage::Catalog) {
//!     let _ = catalog.begin_derived_with_binding(Vec::new(), 0, |_, _| Ok(()));
//! }
//! ```

use super::{COLUMNS, Catalog, objects, read_bucket, unavailable};
use crate::{
    bucket::{BucketAccess, BucketRecord, CatalogError},
    clock::Clock,
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord,
        UploadChunk,
    },
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params_from_iter};
use std::{any::Any, fmt, io, num::NonZeroI64, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

pub const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_BUCKETS: usize = 1000;
pub const MAX_PAGE: usize = 256;

#[path = "imported_identity.rs"]
pub mod imported_identity;
pub use imported_identity::ImportedIdentity;

#[path = "import_publication.rs"]
mod import_publication;

#[cfg(test)]
#[path = "admin_tests.rs"]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Api,
    Seed,
    BucketInit,
    SyncIngest,
    Import,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminGrant {
    origin: Origin,
}

impl AdminGrant {
    pub fn origin(&self) -> Origin {
        self.origin
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketName(String);
impl BucketName {
    pub fn new(value: impl Into<String>) -> Result<Self, AdminError> {
        let value = value.into();
        crate::json::validate_bucket_name(&value)
            .map_err(|error| AdminError::Invalid(error.message))?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectName(String);
impl ObjectName {
    pub fn new(value: impl Into<String>) -> Result<Self, AdminError> {
        let value = value.into();
        crate::json::validate_object_name(&value)
            .map_err(|error| AdminError::Invalid(error.message))?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation(NonZeroI64);
impl Generation {
    pub fn new(value: i64) -> Result<Self, AdminError> {
        NonZeroI64::new(value)
            .filter(|value| value.get() > 0)
            .map(Self)
            .ok_or_else(|| AdminError::Invalid("generation must be positive".into()))
    }
    pub fn get(self) -> i64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationCondition {
    Absent,
    Is(Generation),
}
impl GenerationCondition {
    fn conditions(self) -> ObjectConditions {
        ObjectConditions {
            if_generation_match: Some(match self {
                Self::Absent => 0,
                Self::Is(value) => value.get(),
            }),
            ..Default::default()
        }
    }
}

/// Keeps caller-owned metadata but validates the complete batch before any mutation.
#[derive(Clone, Debug)]
pub struct BucketDefinition(BucketRecord);
impl BucketDefinition {
    pub fn new(record: BucketRecord) -> Result<Self, AdminError> {
        BucketName::new(record.name.clone())?;
        crate::json::validate_project(&record.project)
            .map_err(|error| AdminError::Invalid(error.message))?;
        crate::bucket::validate_cors(&record.cors).map_err(AdminError::from)?;
        crate::bucket::validate_storage_class(&record.storage_class).map_err(AdminError::from)?;
        if record.metageneration <= 0
            || !matches!(record.location.as_str(), "US" | "EU" | "ASIA")
            || record.location_type != "multi-region"
            || record.labels.len() > 64
            || record.labels.iter().any(|(key, value)| {
                !crate::bucket::valid_label(key, true) || !crate::bucket::valid_label(value, false)
            })
        {
            return Err(AdminError::Invalid("invalid bucket metadata".into()));
        }
        super::unix_micros(record.time_created).map_err(AdminError::from)?;
        super::unix_micros(record.updated).map_err(AdminError::from)?;
        Ok(Self(record))
    }
    pub fn default_for(
        name: &BucketName,
        project: &str,
        clock: &dyn Clock,
    ) -> Result<Self, AdminError> {
        let input = serde_json::json!({"name": name.as_str()}).to_string();
        let record = crate::json::parse_insert(input.as_bytes(), project, clock.now())
            .map_err(|error| AdminError::Invalid(error.message))?;
        Self::new(record)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EnsureBucket {
    Created(BucketRecord),
    Existing(BucketRecord),
}
impl EnsureBucket {
    pub fn record(&self) -> &BucketRecord {
        match self {
            Self::Created(record) | Self::Existing(record) => record,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishMode {
    Replace,
    InheritFromReplaced,
    SkipIfSameHash,
    InheritAndSkipIfSameHash,
}

#[path = "publication.rs"]
mod publication;
pub(super) use publication::FileWorkers;
pub use publication::{
    BatchPublication, BatchResult, ImportedBatchResult, ImportedPublication, StagedObject,
};

pub struct Publication {
    pub content_type: String,
    pub metadata: ObjectMetadataUpdate,
    pub expected: ExpectedHashes,
    pub condition: Option<GenerationCondition>,
    pub max_bytes: u64,
    pub mode: PublishMode,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdminError {
    Busy { retry_after: Duration },
    DiskFull,
    Closed,
    Precondition,
    Hold,
    SourceChanged { fields: Vec<String> },
    Corrupt(String),
    Io(io::ErrorKind),
    NotFound,
    Invalid(String),
    Checksum,
    TooLarge,
    Forbidden,
    BodyAborted(String),
    Backend(String),
}
impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "administrative operation: {self:?}")
    }
}
impl std::error::Error for AdminError {}
impl From<io::Error> for AdminError {
    fn from(error: io::Error) -> Self {
        Self::from_kind(error.kind())
    }
}
impl AdminError {
    fn from_kind(kind: io::ErrorKind) -> Self {
        if kind == io::ErrorKind::StorageFull {
            Self::DiskFull
        } else {
            Self::Io(kind)
        }
    }
}
impl From<CatalogError> for AdminError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::Io(kind) => Self::from_kind(kind),
            CatalogError::Corrupt(reason) => Self::Corrupt(reason),
            CatalogError::PreconditionFailed => Self::Precondition,
            CatalogError::NotFound => Self::NotFound,
            CatalogError::Forbidden => Self::Forbidden,
            CatalogError::InvalidMetadata(reason) | CatalogError::UnsupportedMetadata(reason) => {
                Self::Invalid(reason)
            }
            error => Self::Backend(error.to_string()),
        }
    }
}
impl From<ObjectError> for AdminError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::Io(kind) => Self::from_kind(kind),
            ObjectError::Corrupt(reason) => Self::Corrupt(reason),
            ObjectError::InvalidFixedMetadata(reason)
            | ObjectError::UnsupportedMetadata(reason) => Self::Invalid(reason),
            ObjectError::PreconditionFailed => Self::Precondition,
            ObjectError::ActiveHold => Self::Hold,
            ObjectError::BucketNotFound | ObjectError::ObjectNotFound => Self::NotFound,
            ObjectError::ChecksumMismatch => Self::Checksum,
            ObjectError::TooLarge => Self::TooLarge,
            ObjectError::InvalidMetadata => Self::Invalid(error.to_string()),
            ObjectError::Forbidden | ObjectError::OverwriteForbidden => Self::Forbidden,
            ObjectError::BodyAborted(reason) => Self::BodyAborted(reason),
            error => Self::Backend(error.to_string()),
        }
    }
}

pub(super) fn io_kind(error: &dyn Any) -> Option<io::ErrorKind> {
    if let Some(error) = error.downcast_ref::<io::Error>() {
        return Some(error.kind());
    }
    if let Some(CatalogError::Io(kind)) = error.downcast_ref::<CatalogError>() {
        return Some(*kind);
    }
    if let Some(ObjectError::Io(kind)) = error.downcast_ref::<ObjectError>() {
        return Some(*kind);
    }
    if let Some(rusqlite::Error::SqliteFailure(error, _)) = error.downcast_ref::<rusqlite::Error>()
        && error.code == rusqlite::ErrorCode::DiskFull
    {
        return Some(io::ErrorKind::StorageFull);
    }
    None
}

pub(super) fn corrupt_reason(error: &dyn Any) -> Option<String> {
    if let Some(CatalogError::Corrupt(reason)) = error.downcast_ref::<CatalogError>() {
        return Some(reason.clone());
    }
    if let Some(ObjectError::Corrupt(reason)) = error.downcast_ref::<ObjectError>() {
        return Some(reason.clone());
    }
    let error = error.downcast_ref::<rusqlite::Error>()?;
    match error {
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(
                failure.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            Some(error.to_string())
        }
        error @ (rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::Utf8Error(..)) => Some(error.to_string()),
        _ => None,
    }
}

#[derive(Clone)]
pub struct Admin {
    catalog: Catalog,
    grant: AdminGrant,
}
impl Catalog {
    /// Operator-owned input only; API handlers must continue through their auth profile.
    pub fn admin(&self, origin: Origin) -> Admin {
        Admin {
            catalog: self.clone(),
            grant: AdminGrant { origin },
        }
    }

    pub(super) async fn wait_admin(
        &self,
        slots: Arc<Semaphore>,
        deadline: tokio::time::Instant,
    ) -> Result<OwnedSemaphorePermit, AdminError> {
        let shutdown = self.0.admin_shutdown.notified();
        tokio::pin!(shutdown);
        shutdown.as_mut().enable();
        if !self.is_healthy() {
            return Err(AdminError::Closed);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AdminError::Busy {
                retry_after: Duration::from_secs(1),
            });
        }
        let _queue = self
            .0
            .admin_waiters
            .clone()
            .try_acquire_owned()
            .map_err(|_| AdminError::Busy {
                retry_after: Duration::from_secs(1),
            })?;
        tokio::select! {
            biased;
            _ = shutdown => Err(AdminError::Closed),
            result = tokio::time::timeout_at(deadline, slots.acquire_owned()) => {
                let permit = result.map_err(|_| AdminError::Busy { retry_after: Duration::from_secs(1) })?.map_err(|_| AdminError::Closed)?;
                if !self.is_healthy() { return Err(AdminError::Closed); }
                if tokio::time::Instant::now() >= deadline { return Err(AdminError::Busy { retry_after: Duration::from_secs(1) }); }
                Ok(permit)
            }
        }
    }

    pub(super) async fn run_admin<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, AdminError> {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        self.run_admin_until(deadline, operation).await
    }

    async fn run_admin_until<T: Send + 'static>(
        &self,
        deadline: tokio::time::Instant,
        operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, AdminError> {
        let admission = self
            .wait_admin(self.0.admin_reads.clone(), deadline)
            .await?;
        let execution = self.wait_admin(self.0.execution.clone(), deadline).await?;
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let (_admission, _execution) = (admission, execution);
            let result = match inner.resources.lock() {
                Ok(mut resources) => match resources.as_mut() {
                    Some(resources) => {
                        operation(&mut resources.connection).map_err(AdminError::from)
                    }
                    None => Err(AdminError::Closed),
                },
                Err(error) => Err(AdminError::Corrupt(error.to_string())),
            };
            drop(inner);
            result
        })
        .await
        .map_err(|error| AdminError::Backend(error.to_string()))?
    }
}

impl Admin {
    pub fn origin(&self) -> Origin {
        self.grant.origin()
    }

    pub(crate) async fn seed_complete(&self) -> Result<bool, AdminError> {
        self.catalog
            .run_admin(|connection| {
                connection
                    .query_row(
                        "SELECT complete FROM startup_seed WHERE singleton = 1",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(unavailable)
            })
            .await
    }

    pub(crate) async fn finish_seed(&self) -> Result<(), AdminError> {
        self.catalog
            .run_admin(|connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction
                    .execute(
                        "UPDATE startup_seed SET complete = 1 WHERE singleton = 1",
                        [],
                    )
                    .map_err(unavailable)?;
                transaction.commit().map_err(unavailable)
            })
            .await
    }

    pub async fn ensure_bucket(
        &self,
        record: BucketDefinition,
    ) -> Result<EnsureBucket, AdminError> {
        Ok(self.ensure_buckets(vec![record]).await?.remove(0))
    }

    pub async fn ensure_buckets(
        &self,
        records: Vec<BucketDefinition>,
    ) -> Result<Vec<EnsureBucket>, AdminError> {
        if records.len() > MAX_BUCKETS {
            return Err(AdminError::Invalid(
                "bucket batch exceeds 1000 entries".into(),
            ));
        }
        let mut names = std::collections::HashSet::new();
        if records.iter().any(|record| !names.insert(&record.0.name)) {
            return Err(AdminError::Invalid("duplicate bucket name".into()));
        }
        let origin = self.origin();
        self.catalog
            .run_admin(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(unavailable)?;
                let mut results = Vec::with_capacity(records.len());
                let placeholders = std::iter::repeat_n("?", records.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let mut lookup = transaction
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM buckets WHERE name IN ({placeholders})"
                    ))
                    .map_err(unavailable)?;
                let mut existing = lookup
                    .query_map(
                        params_from_iter(records.iter().map(|record| &record.0.name)),
                        read_bucket,
                    )
                    .map_err(unavailable)?
                    .map(|record| record.map(|record| (record.name.clone(), record)))
                    .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>()
                    .map_err(unavailable)?;
                drop(lookup);
                for BucketDefinition(mut record) in records {
                    if let Some(existing) = existing.remove(&record.name) {
                        results.push(EnsureBucket::Existing(existing));
                        continue;
                    }
                    super::insert_bucket_in_transaction(&transaction, &mut record)?;
                    super::change_journal::append(
                        &transaction,
                        origin,
                        super::change_journal::ChangeKind::BucketCreate,
                        &record.name,
                        None,
                    )?;
                    results.push(EnsureBucket::Created(record));
                }
                transaction.commit().map_err(unavailable)?;
                Ok(results)
            })
            .await
    }

    pub async fn get_bucket(&self, name: &BucketName) -> Result<Option<BucketRecord>, AdminError> {
        let name = name.0.clone();
        self.catalog
            .run_admin(move |connection| {
                connection
                    .query_row(
                        &format!("SELECT {COLUMNS} FROM buckets WHERE name = ?1"),
                        [&name],
                        read_bucket,
                    )
                    .optional()
                    .map_err(unavailable)
            })
            .await
    }

    pub async fn list_buckets(
        &self,
        project: &str,
        after: Option<&BucketName>,
        limit: usize,
    ) -> Result<super::BucketListPage, AdminError> {
        crate::json::validate_project(project)
            .map_err(|error| AdminError::Invalid(error.message))?;
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AdminError::Invalid(
                "management page limit must be 1..=256".into(),
            ));
        }
        let project = project.to_owned();
        let after = after.map(|name| name.0.clone());
        self.catalog
            .run_admin(move |connection| {
                super::bucket_list_page(connection, project, String::new(), after, limit)
            })
            .await
    }

    pub async fn publish_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        request: Publication,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<ObjectRecord, AdminError> {
        self.publish_with_identity(bucket, name, request, None, body)
            .await
    }

    async fn publish_with_identity(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        request: Publication,
        identity: Option<ImportedIdentity>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<ObjectRecord, AdminError> {
        crate::object::validate_fixed_metadata("contentType", &request.content_type)
            .map_err(AdminError::from)?;
        request.metadata.validate().map_err(AdminError::from)?;
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        let permit = self
            .catalog
            .wait_admin(self.catalog.0.admin_uploads.clone(), deadline)
            .await?;
        let conditions = request
            .condition
            .map(GenerationCondition::conditions)
            .unwrap_or_default();
        let access = BucketAccess::Admin(self.grant.clone());
        self.check_upload_until(bucket, name, request.condition, deadline)
            .await?;
        self.catalog
            .put_object_with_mode(
                bucket.as_str(),
                name.as_str(),
                request.content_type,
                request.expected,
                conditions,
                request.max_bytes,
                body,
                permit,
                request.metadata,
                access,
                request.mode,
                identity,
            )
            .await
            .map_err(AdminError::from)
    }

    pub async fn check_upload(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        condition: Option<GenerationCondition>,
    ) -> Result<(), AdminError> {
        self.check_upload_until(
            bucket,
            name,
            condition,
            tokio::time::Instant::now() + WAIT_TIMEOUT,
        )
        .await
    }

    async fn check_upload_until(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        condition: Option<GenerationCondition>,
        deadline: tokio::time::Instant,
    ) -> Result<(), AdminError> {
        let bucket = bucket.0.clone();
        let name = name.0.clone();
        let access = BucketAccess::Admin(self.grant.clone());
        self.catalog
            .run_admin_until(deadline, move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                let result = (|| {
                    objects::authorized_bucket(&transaction, &bucket, &access)?;
                    let previous = objects::lookup(&transaction, &bucket, &name)?;
                    objects::check_publication(
                        previous.as_ref(),
                        condition
                            .map(GenerationCondition::conditions)
                            .unwrap_or_default(),
                        true,
                    )
                })();
                Ok(result)
            })
            .await?
            .map_err(AdminError::from)
    }

    pub async fn get_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        generation: Option<Generation>,
    ) -> Result<ObjectRecord, AdminError> {
        let bucket = bucket.0.clone();
        let name = name.0.clone();
        let access = BucketAccess::Admin(self.grant.clone());
        self.catalog
            .run_admin(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                Ok(objects::selected(
                    &transaction,
                    &bucket,
                    &name,
                    generation.map(Generation::get),
                    ObjectConditions::default(),
                    true,
                    &access,
                )
                .map(|(record, _)| record))
            })
            .await?
            .map_err(AdminError::from)
    }

    pub async fn open_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        generation: Option<Generation>,
    ) -> Result<(ObjectRecord, super::ObjectReader), AdminError> {
        let bucket = bucket.0.clone();
        let name = name.0.clone();
        let access = BucketAccess::Admin(self.grant.clone());
        let data_dir = self.catalog.0.data_dir.clone();
        self.catalog
            .run_admin(move |connection| {
                Ok(objects::open_selected(
                    connection,
                    &data_dir,
                    &bucket,
                    &name,
                    generation.map(Generation::get),
                    ObjectConditions::default(),
                    &access,
                ))
            })
            .await?
            .map_err(AdminError::from)
    }

    pub async fn list_objects(
        &self,
        bucket: &BucketName,
        mut query: super::ObjectListQuery,
        limit: usize,
    ) -> Result<super::ObjectListPage, AdminError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AdminError::Invalid(
                "management page limit must be 1..=256".into(),
            ));
        }
        if !query.bucket.is_empty() && query.bucket != bucket.as_str() {
            return Err(AdminError::Invalid("list query bucket mismatch".into()));
        }
        query.bucket = bucket.0.clone();
        let access = BucketAccess::Admin(self.grant.clone());
        self.catalog
            .run_admin(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                Ok((|| {
                    objects::authorized_bucket(&transaction, &query.bucket, &access)?;
                    objects::list_page(&transaction, &query, limit)
                })())
            })
            .await?
            .map_err(AdminError::from)
    }

    pub async fn delete_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        condition: GenerationCondition,
    ) -> Result<(), AdminError> {
        self.delete_with_sync(bucket, name, condition, None).await
    }

    pub async fn delete_synced_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        condition: GenerationCondition,
        binding: super::sync_state::SyncBinding,
    ) -> Result<(), AdminError> {
        self.delete_with_sync(bucket, name, condition, Some(binding))
            .await
    }

    async fn delete_with_sync(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        condition: GenerationCondition,
        binding: Option<super::sync_state::SyncBinding>,
    ) -> Result<(), AdminError> {
        let bucket = bucket.0.clone();
        let name = name.0.clone();
        let access = BucketAccess::Admin(self.grant.clone());
        let clock = self.catalog.0.clock.clone();
        self.catalog
            .run_admin(move |connection| {
                Ok(objects::delete_selected(
                    connection,
                    clock.as_ref(),
                    &bucket,
                    &name,
                    None,
                    condition.conditions(),
                    &access,
                    binding.as_ref(),
                ))
            })
            .await?
            .map_err(AdminError::from)?;
        self.catalog.reclaim_after_commit().await;
        Ok(())
    }
}
