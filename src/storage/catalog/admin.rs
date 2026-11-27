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

use super::{Catalog, Work, objects, read_bucket, select_bucket, unavailable};
use crate::bucket::name::BucketKey;
use crate::cause::{BackendCause, IoCause, Link, TypedSource};
use crate::object::ObjectKey;
use crate::storage::catalog::sql;
use crate::{
    bucket::{BucketAccess, BucketRecord, CatalogError, DefaultBucketError},
    clock::Clock,
    fsutil::{FsError, IdentityField},
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord,
        ObjectTarget, UploadChunk,
    },
};
use rusqlite::Connection;
use std::{fmt, io, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

#[derive(Debug)]
#[must_use = "admission covers the operation and its owned cleanup"]
pub(super) struct AdminPermit {
    _slot: OwnedSemaphorePermit,
    _task: Arc<crate::task::TaskToken>,
}

pub(crate) const MAX_PAGE: usize = 256;

pub(crate) mod imported_identity;
pub(crate) use imported_identity::ImportedIdentity;

mod import_publication;
pub(crate) mod initialization;
mod purge;
pub(crate) use purge::PurgeError;
pub(crate) mod replay;
mod reset;
pub(super) use reset::validate_reset_epoch;
pub(crate) mod seed_records;
pub(crate) mod seed_watch;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Api,
    Seed,
    BucketInit,
    Import,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdminGrant {
    origin: Origin,
    notification_mode: crate::notification::NotificationMode,
}

#[derive(Clone, Copy)]
pub enum LocalAdmin {
    Disabled,
    Enabled,
}

pub(super) fn startup_grant(mode: LocalAdmin) -> Option<AdminGrant> {
    matches!(mode, LocalAdmin::Enabled).then_some(AdminGrant {
        origin: Origin::Seed,
        notification_mode: crate::notification::NotificationMode::Emit,
    })
}

#[cfg(test)]
impl AdminGrant {
    /// The grant that startup seeding holds when local administration is enabled.
    pub(crate) fn seed() -> Self {
        Self {
            origin: Origin::Seed,
            notification_mode: crate::notification::NotificationMode::Emit,
        }
    }
}

impl AdminGrant {
    pub(crate) fn origin(&self) -> Origin {
        self.origin
    }
    /// What the grant's operations report. An import reproduces its source and reports nothing,
    /// whatever mode was stored.
    pub(crate) fn notification_mode(&self) -> crate::notification::NotificationMode {
        match self.origin {
            Origin::Import => crate::notification::NotificationMode::Suppress,
            Origin::Api | Origin::Seed | Origin::BucketInit => self.notification_mode,
        }
    }
    /// The bucket access every administrative operation of this grant checks.
    pub(crate) fn access(&self) -> BucketAccess {
        BucketAccess::Admin(self.clone())
    }
}

pub use crate::bucket::name::{BucketName, InvalidBucketName};
pub use crate::object::generation::{Generation, InvalidGeneration};
pub use crate::object::name::{InvalidObjectName, ObjectName};

// The administration API's constructors keep its error type: seed and fixtures report
// `AdminError`'s text. They live beside `AdminError` rather than beside the types.
impl BucketName {
    pub fn new(value: impl Into<String>) -> Result<Self, AdminError> {
        Ok(Self::try_from(value.into())?)
    }
}
impl ObjectName {
    pub fn new(value: impl Into<String>) -> Result<Self, AdminError> {
        Ok(Self::try_from(value.into())?)
    }
}
impl Generation {
    pub fn new(value: i64) -> Result<Self, AdminError> {
        Ok(Self::try_from(value)?)
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
    pub(crate) fn record(&self) -> &BucketRecord {
        &self.0
    }
    pub fn new(record: BucketRecord) -> Result<Self, AdminError> {
        BucketName::new(record.name.clone())?;
        crate::auth::validate_project_id(&record.project)?;
        crate::bucket::validate_cors(&record.cors)?;
        if record.metageneration <= 0
            || record.labels.len() > crate::bucket::MAX_LABELS
            || record.labels.iter().any(|(key, value)| {
                !crate::bucket::valid_label(key, crate::bucket::LabelPart::Key)
                    || !crate::bucket::valid_label(value, crate::bucket::LabelPart::Value)
            })
        {
            return Err(AdminError::Invalid("invalid bucket metadata".into()));
        }
        super::unix_micros(record.time_created)?;
        super::unix_micros(record.updated)?;
        Ok(Self(record))
    }
    pub fn default_for(
        name: &BucketName,
        project: &str,
        clock: &dyn Clock,
    ) -> Result<Self, AdminError> {
        let record = BucketRecord::default_for(name, project, clock.now())?;
        Self::new(record)
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
pub(in crate::storage) use publication::NestedFileJob;
pub(super) use publication::apply_acl;
pub(crate) use publication::{BatchPublication, ImportedPublication};
pub(crate) use publication::{LiveOutcome, LiveWalk, Removed, SeedBatch, SeedMode};

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
    Busy {
        retry_after: Duration,
    },
    DiskFull(IoCause),
    Closed,
    Precondition,
    Hold,
    SeedProjectMismatch,
    /// Fixture metadata exceeded the budget that `max_seed_entries` sets, in MiB.
    SeedMetadataBudget {
        mib: usize,
    },
    SourceChanged {
        fields: Vec<IdentityField>,
    },
    Corrupt(BackendCause),
    Io(IoCause),
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
        f.write_str("administrative operation: ")?;
        match self {
            Self::Busy { retry_after } => write!(f, "Busy {{ retry_after: {retry_after:?} }}"),
            Self::DiskFull(_) => f.write_str("DiskFull"),
            Self::Closed => f.write_str("Closed"),
            Self::Precondition => f.write_str("Precondition"),
            Self::Hold => f.write_str("Hold"),
            // Preserve the diagnostic that preceded the typed project mismatch.
            Self::SeedProjectMismatch => {
                f.write_str("Invalid(\"existing seed bucket belongs to another project\")")
            }
            Self::SeedMetadataBudget { mib } => write!(
                f,
                "Invalid(\"seed metadata exceeds {mib} MiB; raise max_seed_entries\")"
            ),
            Self::SourceChanged { fields } => write!(f, "SourceChanged {{ fields: {fields:?} }}"),
            Self::Corrupt(cause) => write!(f, "Corrupt({cause:?})"),
            Self::Io(cause) => write!(f, "Io({cause:?})"),
            Self::NotFound => f.write_str("NotFound"),
            Self::Invalid(reason) => write!(f, "Invalid({reason:?})"),
            Self::Checksum => f.write_str("Checksum"),
            Self::TooLarge => f.write_str("TooLarge"),
            Self::Forbidden => f.write_str("Forbidden"),
            Self::BodyAborted(cause) => write!(f, "BodyAborted({cause:?})"),
            Self::Backend(cause) => write!(f, "Backend({cause:?})"),
        }
    }
}
impl std::error::Error for AdminError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }
}
impl TypedSource for AdminError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::BodyAborted(cause) | Self::Backend(cause) | Self::Corrupt(cause) => {
                cause.typed_source()
            }
            Self::Io(cause) | Self::DiskFull(cause) => cause.typed_source(),
            Self::Busy { .. }
            | Self::Closed
            | Self::Precondition
            | Self::Hold
            | Self::SeedProjectMismatch
            | Self::SeedMetadataBudget { .. }
            | Self::SourceChanged { .. }
            | Self::NotFound
            | Self::Invalid(_)
            | Self::Checksum
            | Self::TooLarge
            | Self::Forbidden => None,
        }
    }
}
impl From<io::Error> for AdminError {
    fn from(error: io::Error) -> Self {
        Self::from_io(error.into())
    }
}
/// An exhausted RAM blob budget is a full store, as a full disk is.
impl From<crate::storage::memory::BlobError> for AdminError {
    fn from(error: crate::storage::memory::BlobError) -> Self {
        Self::from_io(error.into())
    }
}
impl From<FsError> for AdminError {
    fn from(error: FsError) -> Self {
        match error {
            FsError::Io(error) => error.into(),
            // A kind-only cause: no `io::Error` source joins the reported chain, as before.
            FsError::Unsupported => Self::Io(io::ErrorKind::Unsupported.into()),
        }
    }
}
/// How long a caller turned away by full admin admission waits before trying again.
const BUSY_RETRY_AFTER: Duration = Duration::from_secs(1);

impl AdminError {
    /// Admin admission did not open before the caller's deadline.
    pub(crate) const fn busy() -> Self {
        Self::Busy {
            retry_after: BUSY_RETRY_AFTER,
        }
    }

    /// The failed-pass warning predates the separation of rollback context from its cause.
    pub(crate) fn diagnostic_report(&self) -> impl fmt::Display + '_ {
        fmt::from_fn(|f| {
            #[cfg(test)]
            crate::error::assert_distinct_causes(self);
            let sources = self
                .typed_source()
                .into_iter()
                .flat_map(crate::cause::chain);
            crate::error::join(
                f,
                std::iter::once(self.to_string()).chain(sources.map(|link| link.to_string())),
            )
        })
    }

    /// Wraps a failure outside the catalog's own classification, keeping it as the source.
    pub(crate) fn backend(error: impl Into<crate::cause::Source>) -> Self {
        Self::Backend(BackendCause::from_error(error))
    }

    fn from_io(cause: IoCause) -> Self {
        if cause.kind() == io::ErrorKind::StorageFull {
            Self::DiskFull(cause)
        } else {
            Self::Io(cause)
        }
    }
}
impl From<InvalidBucketName> for AdminError {
    fn from(_: InvalidBucketName) -> Self {
        Self::Invalid("Invalid bucket name".into())
    }
}
impl From<DefaultBucketError> for AdminError {
    fn from(error: DefaultBucketError) -> Self {
        Self::Invalid(error.to_string())
    }
}
impl From<crate::auth::InvalidProjectId> for AdminError {
    fn from(_: crate::auth::InvalidProjectId) -> Self {
        Self::Invalid(crate::auth::InvalidProjectId::MESSAGE.into())
    }
}
impl From<InvalidObjectName> for AdminError {
    fn from(_: InvalidObjectName) -> Self {
        Self::Invalid("Invalid object name".into())
    }
}
impl From<InvalidGeneration> for AdminError {
    fn from(_: InvalidGeneration) -> Self {
        Self::Invalid("generation must be positive".into())
    }
}
/// A closed catalog answers as closed; a job that panicked is a backend failure.
impl From<super::ConnectionUnavailable> for AdminError {
    fn from(error: super::ConnectionUnavailable) -> Self {
        match error {
            super::ConnectionUnavailable::Closed => Self::Closed,
            error @ (super::ConnectionUnavailable::Panicked
            | super::ConnectionUnavailable::Unrecovered(_)) => Self::backend(error),
        }
    }
}

impl From<CatalogError> for AdminError {
    fn from(error: CatalogError) -> Self {
        match error {
            CatalogError::Io(cause) => Self::from_io(cause),
            CatalogError::Corrupt(cause) => Self::Corrupt(cause),
            CatalogError::PreconditionFailed | CatalogError::NotModified => Self::Precondition,
            CatalogError::NotFound => Self::NotFound,
            CatalogError::PublicAccessPrevention
            | CatalogError::Denied(_)
            | CatalogError::Forbidden => Self::Forbidden,
            CatalogError::Metadata(error) => Self::Invalid(error.to_string()),
            error @ (CatalogError::RetentionPolicyLocked(_)
            | CatalogError::RetentionPolicyNotSet) => Self::Invalid(error.to_string()),
            CatalogError::InvalidMetadata(reason) | CatalogError::UnsupportedMetadata(reason) => {
                Self::Invalid(reason)
            }
            error @ (CatalogError::AlreadyExists
            | CatalogError::NotEmpty
            | CatalogError::RestoreConflict
            | CatalogError::NewerSchema { .. }
            | CatalogError::Unavailable(_)) => Self::backend(error),
        }
    }
}
impl From<ObjectError> for AdminError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::Io(cause) => Self::from_io(cause),
            ObjectError::Corrupt(cause) => Self::Corrupt(cause),
            ObjectError::Metadata(error) => Self::Invalid(error.to_string()),
            ObjectError::InvalidFixedMetadata(reason)
            | ObjectError::UnsupportedMetadata(reason) => Self::Invalid(reason),
            ObjectError::PreconditionFailed
            | ObjectError::WriteNotModified
            | ObjectError::SourceNotModified => Self::Precondition,
            ObjectError::ActiveHold(_) | ObjectError::ObjectRetentionNotMet(_) => Self::Hold,
            ObjectError::BucketNotFound | ObjectError::ObjectNotFound => Self::NotFound,
            ObjectError::ChecksumMismatch | ObjectError::HashMismatch(_) => Self::Checksum,
            ObjectError::TooLarge => Self::TooLarge,
            ObjectError::UniformAccessPredefinedAcl
            | ObjectError::UniformAccessAclUpdate
            | ObjectError::SoftDeletePolicyRequired(_) => Self::Invalid(error.to_string()),
            ObjectError::OverwriteForbidden(_)
            | ObjectError::PublicAccessPrevention
            | ObjectError::Denied(_)
            | ObjectError::Forbidden => Self::Forbidden,
            ObjectError::BodyAborted(reason) => Self::BodyAborted(reason),
            error @ (ObjectError::Component(_)
            | ObjectError::UnlockedRetentionOverrideRequired
            | ObjectError::LockedRetention
            | ObjectError::ObjectNotSoftDeleted
            | ObjectError::NotModified
            | ObjectError::InvalidDerivedArgument(_)
            | ObjectError::ComposeClassMismatch { .. }
            | ObjectError::DerivedTransferNotFound
            | ObjectError::DerivedTransferExpired
            | ObjectError::DerivedTransferBusy
            | ObjectError::DerivedTransferIncomplete
            | ObjectError::CustomerEncryption(_)
            | ObjectError::Unavailable(_)) => Self::backend(error),
        }
    }
}

#[derive(Clone)]
pub struct Admin {
    catalog: Catalog,
    grant: AdminGrant,
}

/// Checks a management listing's page size, which every route reports in the same words.
fn management_page_limit(limit: usize) -> Result<usize, AdminError> {
    super::PageLimit::<MAX_PAGE>::try_from(limit)
        .map(super::PageLimit::get)
        .map_err(|_| AdminError::Invalid("management page limit must be 1..=256".into()))
}

impl Catalog {
    pub(crate) fn local_admin(&self) -> Option<Admin> {
        self.0.admin.grant.clone().map(|grant| Admin {
            catalog: self.clone(),
            grant,
        })
    }
    /// Operator-owned input only; API handlers must continue through their auth profile.
    pub fn admin(&self, origin: Origin) -> Admin {
        Admin {
            catalog: self.clone(),
            grant: AdminGrant {
                origin,
                notification_mode: if matches!(origin, Origin::Seed | Origin::Import) {
                    crate::notification::NotificationMode::Suppress
                } else {
                    crate::notification::NotificationMode::Emit
                },
            },
        }
    }

    /// Waits for a slot until `deadline` or until the catalog closes.
    ///
    /// The line needs no limit of its own: the server's connections already bound who can stand in
    /// it, each caller leaves by its deadline, and dropping a wait gives up its place at no cost.
    pub(super) async fn wait_admin(
        &self,
        slots: Arc<Semaphore>,
        deadline: tokio::time::Instant,
    ) -> Result<OwnedSemaphorePermit, AdminError> {
        if !self.is_healthy() {
            return Err(AdminError::Closed);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AdminError::busy());
        }
        tokio::select! {
            biased;
            () = self.0.shutdown.token.cancelled() => Err(AdminError::Closed),
            result = tokio::time::timeout_at(deadline, slots.acquire_owned()) => {
                let permit = result.map_err(|_| AdminError::busy())?.map_err(|_| AdminError::Closed)?;
                if !self.is_healthy() { return Err(AdminError::Closed); }
                if tokio::time::Instant::now() >= deadline { return Err(AdminError::busy()); }
                Ok(permit)
            }
        }
    }

    async fn admit_admin(
        &self,
        slots: Arc<Semaphore>,
        deadline: tokio::time::Instant,
    ) -> Result<AdminPermit, AdminError> {
        let slot = self.wait_admin(slots, deadline).await?;
        let task = self.0.shutdown.tasks.track().ok_or(AdminError::Closed)?;
        if self.is_closing() {
            return Err(AdminError::Closed);
        }
        Ok(AdminPermit {
            _slot: slot,
            _task: task,
        })
    }

    pub(super) async fn run_admin<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, AdminError> {
        self.run_admin_with_error(move |connection| operation(connection).map_err(AdminError::from))
            .await
    }

    /// Unlike `run_admin`, accepts a job that already selects its domain error.
    pub(super) async fn run_admin_with_error<
        T: Send + 'static,
        E: From<AdminError> + Send + 'static,
    >(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        self.run_admin_until(self.admin_deadline(), operation).await
    }

    /// As with [`Self::run`], the execution permit keeps this job apart from an exclusive job.
    async fn run_admin_until<T: Send + 'static, E: From<AdminError> + Send + 'static>(
        &self,
        deadline: tokio::time::Instant,
        operation: impl FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    ) -> Result<T, E> {
        let admission = self
            .admit_admin(self.0.admin.reads.clone(), deadline)
            .await?;
        let execution = self
            .wait_admin(self.0.writer.execution.clone(), deadline)
            .await?;
        let inner = self.0.clone();
        self.0
            .writer
            .executor
            .run(move || {
                let (_admission, _execution) = (admission, execution);
                let result = inner
                    .with_connection(operation)
                    .unwrap_or_else(|error| Err(AdminError::from(error).into()));
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
    pub(crate) fn with_notification_mode(
        mut self,
        mode: crate::notification::NotificationMode,
    ) -> Self {
        self.grant.notification_mode = mode;
        self
    }
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
                sql::exec_on_writer(
                    connection,
                    "UPDATE startup_seed SET complete = 1 WHERE singleton = 1",
                    [],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn ensure_bucket(
        &self,
        record: BucketDefinition,
    ) -> Result<EnsureBucket, AdminError> {
        self.ensure_buckets(vec![record])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| AdminError::Backend("missing ensured bucket".into()))
    }

    /// Publishes every bucket in one transaction. The initialization file that supplies a batch
    /// has already enforced `max_buckets_per_batch`, so a batch needs no second count here.
    pub(crate) async fn ensure_buckets(
        &self,
        records: Vec<BucketDefinition>,
    ) -> Result<Vec<EnsureBucket>, AdminError> {
        require_unique_names(records.iter().map(|record| record.0.name.as_str()))?;
        let default_retention = self.catalog.default_soft_delete_retention();
        self.catalog
            .run_admin(move |connection| {
                let transaction = sql::immediate(connection)?;
                let mut existing = existing_buckets(
                    &transaction,
                    records.iter().map(|record| record.0.name.as_str()),
                )?;
                let mut results = Vec::with_capacity(records.len());
                for BucketDefinition(mut record) in records {
                    if let Some(existing) = existing.remove(&record.name) {
                        results.push(EnsureBucket::Existing(existing));
                        continue;
                    }
                    let creation = super::BucketCreation::default_policy(default_retention);
                    super::create_bucket_in_transaction(&transaction, &mut record, creation)?;
                    results.push(EnsureBucket::Created(record));
                }
                transaction.commit()?;
                Ok(results)
            })
            .await
    }

    pub async fn get_bucket(&self, name: &BucketName) -> Result<Option<BucketRecord>, AdminError> {
        let name = name.as_str().to_owned();
        self.catalog
            .run_admin(move |connection| {
                select_bucket(connection, BucketKey::from_unchecked(name.as_str()))
                    .map_err(unavailable)
            })
            .await
    }

    #[cfg(test)]
    pub(crate) async fn list_buckets(
        &self,
        project: &str,
        after: Option<&BucketName>,
        limit: usize,
    ) -> Result<super::Page<BucketRecord>, AdminError> {
        crate::auth::validate_project_id(project)?;
        let limit = management_page_limit(limit)?;
        let project = project.to_owned();
        let after = after.map(|name| name.as_str().to_owned());
        self.catalog
            .run_admin(move |connection| {
                super::bucket_list_page(connection, Some(&[project]), "", after.as_deref(), limit)
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

    pub(crate) async fn publish_with_identity(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        request: Publication,
        identity: Option<ImportedIdentity>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<ObjectRecord, AdminError> {
        if let Some(identity) = identity {
            identity.validate_publication(&request)?;
        }
        if identity.and_then(ImportedIdentity::time_deleted).is_some() {
            return Err(AdminError::Invalid(
                "archived identities require imported file-batch publication".into(),
            ));
        }
        crate::object::validate_fixed_metadata("contentType", &request.content_type)
            .map_err(ObjectError::from)?;
        request.metadata.validate()?;
        let deadline = self.catalog.admin_deadline();
        let activity = self.catalog.admin_activity(Work::New, deadline).await?;
        let permit = self
            .catalog
            .admit_admin(self.catalog.0.admin.uploads.clone(), deadline)
            .await?;
        let conditions = request
            .condition
            .map_or_default(GenerationCondition::conditions);
        let access = self.grant.access();
        self.check_upload_until(bucket, name, request.condition, deadline)
            .await?;
        self.catalog
            .put_object_with_mode(
                objects::ObjectUpload {
                    key: ObjectKey::from_parts_unchecked(bucket.as_str(), name.as_str()),
                    content_type: request.content_type,
                    expected: request.expected,
                    conditions,
                    max_bytes: request.max_bytes,
                    metadata: request.metadata,
                    access,
                },
                body,
                super::UploadGuard::admin(permit, activity),
                request.mode,
                identity.map_or(
                    objects::PublishIdentity::Assigned,
                    objects::PublishIdentity::Imported,
                ),
                if identity.is_some() {
                    objects::OwnerAssignment::Unknown
                } else {
                    objects::OwnerAssignment::Request
                },
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
        let bucket = bucket.as_str().to_owned();
        let name = name.as_str().to_owned();
        let access = self.grant.access();
        let clock = self.catalog.0.policy.clock.clone();
        self.catalog
            .run_admin_until(deadline, move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                objects::authorized_bucket(&transaction, &bucket, &access)?;
                let previous = objects::lookup(
                    &transaction,
                    ObjectKey::from_parts_unchecked(&bucket, &name),
                )?;
                objects::check_publication(
                    previous.as_ref(),
                    condition.map_or_default(GenerationCondition::conditions),
                    objects::DeletePermission::Granted,
                    clock.now(),
                )
                .map_err(AdminError::from)
            })
            .await
    }

    pub async fn get_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        generation: Option<Generation>,
    ) -> Result<ObjectRecord, AdminError> {
        let bucket = bucket.as_str().to_owned();
        let name = name.as_str().to_owned();
        let access = self.grant.access();
        self.catalog
            .run_admin_with_error(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                objects::selected(
                    &transaction,
                    ObjectKey::from_parts_unchecked(&bucket, &name),
                    generation.map(Generation::get),
                    ObjectConditions::default(),
                    super::objects::RequestKind::Read,
                    &access,
                )
                .map(|stored| stored.record)
                .map_err(AdminError::from)
            })
            .await
    }

    pub async fn open_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        generation: Option<Generation>,
    ) -> Result<(ObjectRecord, super::ObjectReader), AdminError> {
        let target = ObjectTarget::new(
            bucket.as_str().to_owned(),
            name.as_str().to_owned(),
            generation.map(Generation::get),
        );
        let access = self.grant.access();
        let catalog = self.catalog.clone();
        self.catalog
            .run_admin_with_error(move |connection| {
                objects::open_selected(
                    connection,
                    &catalog,
                    &target,
                    ObjectConditions::default(),
                    &access,
                )
                .map_err(AdminError::from)
            })
            .await
    }

    pub async fn list_objects(
        &self,
        bucket: &BucketName,
        mut query: super::ObjectListQuery,
        limit: usize,
    ) -> Result<super::ObjectListPage, AdminError> {
        let limit = management_page_limit(limit)?;
        if !query.bucket.is_empty() && query.bucket != bucket.as_str() {
            return Err(AdminError::Invalid("list query bucket mismatch".into()));
        }
        query.bucket = bucket.as_str().to_owned();
        let access = self.grant.access();
        self.catalog
            .run_admin_with_error(move |connection| {
                let transaction = connection.transaction().map_err(unavailable)?;
                objects::authorized_bucket(&transaction, &query.bucket, &access)?;
                objects::list_page(&transaction, &query, limit).map_err(AdminError::from)
            })
            .await
    }

    pub async fn delete_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        condition: GenerationCondition,
    ) -> Result<(), AdminError> {
        let target = ObjectTarget::new(bucket.as_str().to_owned(), name.as_str().to_owned(), None);
        let access = self.grant.access();
        let clock = self.catalog.0.policy.clock.clone();
        let (purged, result) = self
            .catalog
            .run_admin_with_error(move |connection| {
                objects::delete_selected(
                    connection,
                    clock.as_ref(),
                    &target,
                    condition.conditions(),
                    &access,
                )
                .map_err(AdminError::from)
            })
            .await?;
        Ok(self.catalog.finish_delete(purged, result).await?)
    }
}

const SELECT_EXISTING: &str = concat!(
    "SELECT ",
    super::bucket_columns!(),
    " FROM buckets WHERE name IN (SELECT value FROM json_each(?1))"
);

/// Refuses a batch that names one bucket twice: each name is created or found once.
fn require_unique_names<'a>(mut names: impl Iterator<Item = &'a str>) -> Result<(), AdminError> {
    let mut seen = std::collections::HashSet::new();
    if names.any(|name| !seen.insert(name)) {
        return Err(AdminError::Invalid("duplicate bucket name".into()));
    }
    Ok(())
}

/// The stored buckets among `names`, read with one statement for the whole batch.
fn existing_buckets<'a>(
    transaction: &sql::WriteTx<'_>,
    names: impl Iterator<Item = &'a str>,
) -> Result<std::collections::HashMap<String, BucketRecord>, CatalogError> {
    // The names are bound as one JSON array so the statement text stays static.
    let names = serde_json::to_string(&names.collect::<Vec<_>>()).map_err(unavailable)?;
    transaction
        .prepare_cached(SELECT_EXISTING)?
        .query_map([names], read_bucket)?
        .map(|record| record.map(|record| (record.name.clone(), record)))
        .collect::<rusqlite::Result<_>>()
        .map_err(CatalogError::from)
}
