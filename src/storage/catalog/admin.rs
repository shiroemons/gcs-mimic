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

use super::{Catalog, objects, read_bucket, select_bucket, unavailable};
use crate::storage::failure::BackendCause;
use crate::{
    bucket::{BucketAccess, BucketRecord, CatalogError},
    clock::Clock,
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord,
        UploadChunk,
    },
};
use rusqlite::{Connection, TransactionBehavior};
use std::{fmt, io, num::NonZeroI64, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

pub(crate) const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_BUCKETS: usize = 1000;
pub(crate) const MAX_PAGE: usize = 256;

pub(crate) mod imported_identity;
pub(crate) use imported_identity::ImportedIdentity;

mod import_publication;

#[cfg(test)]
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
pub(crate) struct AdminGrant {
    origin: Origin,
}

impl AdminGrant {
    pub(crate) fn origin(&self) -> Origin {
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
    pub(crate) fn as_str(&self) -> &str {
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
    pub(crate) fn as_str(&self) -> &str {
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
    pub(in crate::storage) fn into_record(self) -> BucketRecord {
        self.0
    }
    pub fn new(record: BucketRecord) -> Result<Self, AdminError> {
        BucketName::new(record.name.clone())?;
        crate::json::validate_project(&record.project)
            .map_err(|error| AdminError::Invalid(error.message))?;
        crate::bucket::validate_cors(&record.cors).map_err(AdminError::from)?;
        crate::bucket::validate_storage_class(&record.storage_class).map_err(AdminError::from)?;
        let (location, location_type) =
            crate::bucket::bucket_location(&record.location).map_err(AdminError::from)?;
        if record.metageneration <= 0
            || record.location != location
            || record.location_type != location_type
            || record.labels.len() > 64
            || record.labels.iter().any(|(key, value)| {
                !crate::bucket::valid_label(key, crate::bucket::LabelPart::Key)
                    || !crate::bucket::valid_label(value, crate::bucket::LabelPart::Value)
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

#[test]
fn bucket_definition_requires_matching_canonical_location_and_type() {
    for location in ["US", "ASIA-NORTHEAST1", "NAM4"] {
        let input = serde_json::json!({"name":"example-bucket", "location":location}).to_string();
        let record = crate::json::parse_insert(input.as_bytes(), "project", std::time::UNIX_EPOCH)
            .expect("supported location");
        assert!(BucketDefinition::new(record.clone()).is_ok());
        for (name, kind) in [
            ("MARS-CENTRAL1", record.location_type.as_str()),
            (location, "invalid-type"),
        ] {
            let mut invalid = record.clone();
            invalid.location = name.into();
            invalid.location_type = kind.into();
            assert!(matches!(
                BucketDefinition::new(invalid),
                Err(AdminError::Invalid(_))
            ));
        }
        let mut invalid = record;
        invalid.location.make_ascii_lowercase();
        assert!(matches!(
            BucketDefinition::new(invalid),
            Err(AdminError::Invalid(_))
        ));
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum EnsureBucket {
    Created(BucketRecord),
    Existing(BucketRecord),
}
impl EnsureBucket {
    #[cfg(test)]
    pub(crate) fn record(&self) -> &BucketRecord {
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

mod publication;
pub(super) use publication::FileWorkers;
pub(crate) use publication::{BatchPublication, ImportedPublication, StagedObject};

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
    BodyAborted(BackendCause),
    Backend(BackendCause),
}
impl fmt::Display for AdminError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "administrative operation: {self:?}")
    }
}
impl std::error::Error for AdminError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BodyAborted(cause) | Self::Backend(cause) => cause.source(),
            _ => None,
        }
    }
}
impl From<io::Error> for AdminError {
    fn from(error: io::Error) -> Self {
        Self::from_kind(error.kind())
    }
}
impl AdminError {
    /// Wraps a failure outside the catalog's own classification, keeping it as the source.
    pub(crate) fn backend(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Backend(BackendCause::from_error(error))
    }

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
            CatalogError::Io(cause) => Self::from_kind(cause.kind()),
            CatalogError::Corrupt(cause) => Self::Corrupt(cause.to_string()),
            CatalogError::PreconditionFailed => Self::Precondition,
            CatalogError::NotFound => Self::NotFound,
            CatalogError::Forbidden => Self::Forbidden,
            error @ (CatalogError::RetentionPolicyLocked(_)
            | CatalogError::RetentionPolicyNotSet) => Self::Invalid(error.to_string()),
            CatalogError::InvalidMetadata(reason) | CatalogError::UnsupportedMetadata(reason) => {
                Self::Invalid(reason)
            }
            error => Self::backend(error),
        }
    }
}
impl From<ObjectError> for AdminError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::Io(cause) => Self::from_kind(cause.kind()),
            ObjectError::Corrupt(cause) => Self::Corrupt(cause.to_string()),
            ObjectError::InvalidFixedMetadata(reason)
            | ObjectError::UnsupportedMetadata(reason) => Self::Invalid(reason),
            ObjectError::PreconditionFailed => Self::Precondition,
            ObjectError::ActiveHold
            | ObjectError::RetentionPolicyNotMet
            | ObjectError::ObjectRetentionNotMet => Self::Hold,
            ObjectError::BucketNotFound | ObjectError::ObjectNotFound => Self::NotFound,
            ObjectError::ChecksumMismatch => Self::Checksum,
            ObjectError::TooLarge => Self::TooLarge,
            ObjectError::InvalidMetadata => Self::Invalid(error.to_string()),
            ObjectError::Forbidden | ObjectError::OverwriteForbidden => Self::Forbidden,
            ObjectError::BodyAborted(reason) => Self::BodyAborted(reason),
            error => Self::backend(error),
        }
    }
}

#[derive(Clone)]
pub struct Admin {
    catalog: Catalog,
    grant: AdminGrant,
}

pub(crate) struct BucketEntry {
    pub bucket: BucketRecord,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "only unit tests read this field")
    )]
    pub incarnation: String,
}

pub(crate) struct BucketPage {
    pub items: Vec<BucketEntry>,
    pub has_more: bool,
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
        self.0
            .executor
            .run(move || {
                let (_admission, _execution) = (admission, execution);
                let result = match inner.with_connection(operation) {
                    Ok(result) => result.map_err(AdminError::from),
                    Err(super::ConnectionUnavailable::Closed) => Err(AdminError::Closed),
                    Err(error) => Err(AdminError::backend(error)),
                };
                drop(inner);
                result
            })
            .await
            .map_err(|error| match error {
                super::executor::ExecutorError::Closed => AdminError::Closed,
                error @ super::executor::ExecutorError::Panicked => AdminError::backend(error),
            })?
    }
}

impl Admin {
    pub(crate) fn origin(&self) -> Origin {
        self.grant.origin()
    }

    #[cfg(test)]
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
                    // xtask-lint: allow(text::uncached_statement) -- marks the startup seed complete once
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

    pub(crate) async fn ensure_buckets(
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
                // The names are bound as one JSON array so the statement text stays static.
                let names = serde_json::to_string(
                    &records
                        .iter()
                        .map(|record| record.0.name.as_str())
                        .collect::<Vec<_>>(),
                )
                .map_err(unavailable)?;
                let mut lookup = transaction
                    .prepare_cached(concat!(
                        "SELECT ",
                        super::bucket_columns!(),
                        " FROM buckets WHERE name IN (SELECT value FROM json_each(?1))"
                    ))
                    .map_err(unavailable)?;
                let mut existing = lookup
                    .query_map([names], read_bucket)
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
            .run_admin(move |connection| select_bucket(connection, &name).map_err(unavailable))
            .await
    }

    #[cfg(test)]
    pub(crate) async fn list_buckets(
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
                super::bucket_list_page(
                    connection,
                    Some(vec![project]),
                    String::new(),
                    after,
                    limit,
                )
            })
            .await
    }

    /// Binary keyset across every owner; continue after the last returned bucket name.
    pub(crate) async fn list_all_buckets(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<BucketPage, AdminError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AdminError::Invalid(
                "management page limit must be 1..=256".into(),
            ));
        }
        let after = after.unwrap_or("").to_owned();
        self.catalog
            .run_admin(move |connection| {
                let mut statement = connection
                    .prepare_cached(concat!(
                        "SELECT ",
                        super::bucket_columns!(),
                        ", i.incarnation FROM buckets
                 LEFT JOIN bucket_incarnations i ON i.bucket = buckets.name
                 WHERE name > ?1 ORDER BY name LIMIT ?2"
                    ))
                    .map_err(unavailable)?;
                let mut rows = statement
                    .query(rusqlite::params![after, limit as i64 + 1])
                    .map_err(unavailable)?;
                let mut items = Vec::new();
                let mut remaining = super::BUCKET_LIST_METADATA_BUDGET;
                let mut has_more = false;
                while let Some(row) = rows.next().map_err(unavailable)? {
                    let incarnation = row
                        .get_ref("incarnation")
                        .map_err(unavailable)?
                        .as_str()
                        .map_err(unavailable)?;
                    let charge = super::bucket_metadata_charge(row)?
                        .saturating_add(incarnation.len().saturating_mul(8));
                    if items.len() == limit || (!items.is_empty() && charge > remaining) {
                        has_more = true;
                        break;
                    }
                    items.push(BucketEntry {
                        bucket: read_bucket(row).map_err(unavailable)?,
                        incarnation: incarnation.to_owned(),
                    });
                    remaining = remaining.saturating_sub(charge);
                }
                Ok(BucketPage { items, has_more })
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
                objects::ObjectUpload {
                    bucket: bucket.as_str(),
                    name: name.as_str(),
                    content_type: request.content_type,
                    expected: request.expected,
                    conditions,
                    max_bytes: request.max_bytes,
                    metadata: request.metadata,
                    access,
                },
                body,
                permit.into(),
                request.mode,
                identity,
            )
            .await
            .map_err(AdminError::from)
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
        let clock = self.catalog.0.clock.clone();
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
                        objects::DeletePermission::Granted,
                        clock.now(),
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
                    super::objects::RequestKind::Read,
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

    #[cfg(test)]
    pub(crate) async fn delete_synced_object(
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
                    objects::ObjectSelector {
                        bucket: &bucket,
                        name: &name,
                        generation: None,
                        conditions: condition.conditions(),
                    },
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
