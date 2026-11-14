#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{
    Blocker, BucketRecord, CleanupOperation, ImportError, ImportFailure, ObjectRecord, Options,
    Page, Record, SourceChange, Summary, codec, scan_with_baseline,
};
use crate::{
    bucket::CatalogError,
    fsutil::IdentityField,
    storage::{
        Catalog,
        admin::{Admin, AdminError, BatchPublication, BucketDefinition, ImportedIdentity, Origin},
        import_journal::{ImportBinding, ImportElement, ImportProgress, ResumeError},
    },
};
use ring::digest::{Context, SHA256, digest};
use std::{
    borrow::Cow,
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
mod tests;

fn invalid(message: impl Into<Cow<'static, str>>) -> ImportError {
    ImportError::Invalid(message.into())
}

/// The identity a destination's journal records: the source and every option that decides what
/// a scan of it finds.
fn source_identity(options: &Options, source: &str) -> [u8; 32] {
    let mut context = Context::new(&SHA256);
    context.update(b"gcs-mimic-import-binding\0\x01");
    if options.source_format == super::SourceFormat::Snapshot {
        context.update(b"memory-snapshot\0\x01");
    }
    let fields = super::option_fields(options);
    let fields = fields.each_ref().map(Vec::as_slice);
    super::digest_fields(
        &mut context,
        "source",
        source,
        &[
            fields[0],
            &[super::FINGERPRINT_FORMAT],
            fields[1],
            fields[2],
            fields[3],
            fields[4],
            fields[5],
        ],
    );
    let mut identity = [0; 32];
    identity.copy_from_slice(context.finish().as_ref());
    identity
}

pub(super) fn sha(bytes: &[u8]) -> [u8; 32] {
    let mut result = [0; 32];
    result.copy_from_slice(digest(&SHA256, bytes).as_ref());
    result
}

fn element(record: &Record, ordinal: i64) -> Option<ImportElement> {
    let mut context = Context::new(&SHA256);
    context.update(b"gcs-mimic-import-element\0\x01");
    let (inode, ctime_ns) = match record {
        Record::Bucket(bucket) => {
            super::digest_fields(
                &mut context,
                "bucket",
                &bucket.path,
                &[bucket.name.as_str().as_bytes(), &bucket.attributes_digest],
            );
            (None, None)
        }
        Record::Object(object) => {
            let fingerprint = &object.fingerprint;
            super::digest_fields(
                &mut context,
                "object",
                &object.path,
                &[
                    object.bucket.as_str().as_bytes(),
                    object.name.as_str().as_bytes(),
                    &object.attributes_digest,
                    &fingerprint.size.to_be_bytes(),
                    &fingerprint.mtime_ns.to_be_bytes(),
                ],
            );
            (fingerprint.inode, fingerprint.ctime_ns)
        }
        Record::Directory { .. } => return None,
    };
    let mut fingerprint = [0; 32];
    fingerprint.copy_from_slice(context.finish().as_ref());
    Some(ImportElement {
        ordinal,
        fingerprint,
        inode,
        ctime_ns,
    })
}

/// Whether a scan records stat observations to the baseline or verifies them against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BaselineMode {
    Record,
    Verify,
}

/// Whether an import starts in an empty destination or resumes an unfinished one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImportStart {
    Fresh,
    Resume,
}

/// This execution's disk-backed observations are never resume authority.
pub(super) struct Baseline {
    file: BaselineIo,
}
enum BaselineIo {
    Record(BufWriter<File>),
    Verify(BufReader<File>),
}

impl Baseline {
    pub(super) fn open(mut file: File, mode: BaselineMode) -> io::Result<Self> {
        file.rewind()?;
        let file = match mode {
            BaselineMode::Record => {
                file.set_len(0)?;
                BaselineIo::Record(BufWriter::new(file))
            }
            BaselineMode::Verify => BaselineIo::Verify(BufReader::new(file)),
        };
        Ok(Self { file })
    }

    pub(super) fn finish(&mut self) -> io::Result<()> {
        if let BaselineIo::Record(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }

    fn create() -> io::Result<File> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let path = std::env::temp_dir().join(format!(
            "gcs-mimic-import-{}",
            crate::hashing::encode_lower(&random)
        ));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options
                .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_DELETE_ON_CLOSE);
        }
        let file = options.open(&path)?;
        #[cfg(unix)]
        fs::remove_file(path)?;
        Ok(file)
    }

    pub(super) fn check(&mut self, record: &Record) -> Result<(), ImportFailure> {
        let Some(element) = element(record, 0) else {
            return Ok(());
        };
        let mut bytes = [0; 66];
        bytes[..32].copy_from_slice(&element.fingerprint);
        bytes[32] = u8::from(element.inode.is_some());
        bytes[33..41].copy_from_slice(&element.inode.unwrap_or_default().to_be_bytes());
        bytes[41] = u8::from(element.ctime_ns.is_some());
        bytes[42..58].copy_from_slice(&element.ctime_ns.unwrap_or_default().to_be_bytes());
        if let Record::Object(object) = record {
            bytes[58..]
                .copy_from_slice(&object.fingerprint.device.unwrap_or_default().to_be_bytes());
        }
        match &mut self.file {
            BaselineIo::Record(file) => file.write_all(&bytes)?,
            BaselineIo::Verify(file) => {
                let mut expected = [0; 66];
                if let Err(error) = file.read_exact(&mut expected) {
                    return Err(if error.kind() == io::ErrorKind::UnexpectedEof {
                        ImportFailure::SourceChanged {
                            fields: vec![super::ChangedField::Metadata],
                        }
                    } else {
                        error.into()
                    });
                }
                if expected != bytes {
                    return Err(ImportFailure::SourceChanged {
                        fields: if expected[..32] != bytes[..32] {
                            vec![super::ChangedField::Metadata]
                        } else {
                            vec![super::ChangedField::Inode, super::ChangedField::Ctime]
                        },
                    });
                }
            }
        }
        Ok(())
    }
}

pub(super) fn time(micros: i64) -> Result<SystemTime, codec::TimestampError> {
    let duration = Duration::from_micros(micros.unsigned_abs());
    if micros >= 0 {
        UNIX_EPOCH.checked_add(duration)
    } else {
        UNIX_EPOCH.checked_sub(duration)
    }
    .ok_or(codec::TimestampError::OUTSIDE_PLATFORM_RANGE)
}

impl ObjectRecord {
    /// The source identity publication records. The scan parsed every time already and blocked
    /// any this platform's clock cannot hold, so these conversions hold for a scanned record.
    fn identity(&self) -> Result<ImportedIdentity, ImportError> {
        let identity = ImportedIdentity::new(
            self.spec.generation,
            time(self.created_micros)?,
            time(self.updated_micros)?,
        )?;
        Ok(match self.deleted_micros {
            Some(deleted) => identity.archived(time(deleted)?)?,
            None => identity,
        })
    }
}

fn findings(page: &Page, diagnostics: &mut impl Write) -> Result<(), ImportError> {
    crate::diagnostics::Diagnostics::from(page).write(diagnostics)?;
    Ok(())
}

async fn metadata_scan(
    options: &Options,
    baseline: &File,
    mode: BaselineMode,
    diagnostics: &mut impl Write,
) -> Result<Summary, ImportError> {
    let (mut receive, worker) = scan_with_baseline(
        options.clone(),
        crate::fsutil::read_source_metadata,
        Some((baseline.try_clone()?, mode)),
        super::ScanPurpose::Inspect,
    )?;
    let result = async {
        while let Some(page) = receive.recv().await {
            findings(&page, diagnostics)?;
        }
        Ok::<_, ImportError>(())
    }
    .await;
    drop(receive);
    let joined = join_scan(worker).await?;
    result?;
    let summary = joined?;
    require_eligible(&summary)?;
    Ok(summary)
}

/// Refuses a scan that found blockers, as its most severe kind: a source that needs repair is
/// terminal, while unreadable entries and stat drift alone are retried.
fn require_eligible(summary: &Summary) -> Result<(), ImportError> {
    if summary.eligible {
        return Ok(());
    }
    Err(match summary.worst_blocker {
        Some(Blocker::Io) => ImportError::SourceUnreadable,
        Some(Blocker::StatDrift) => ImportFailure::SourceChanged {
            fields: vec![super::ChangedField::Inode, super::ChangedField::Ctime],
        }
        .into(),
        Some(Blocker::Source) | None => invalid(
            "import preflight found blockers; restore an immutable supported snapshot and retry",
        ),
    })
}

fn same_export(before: &Summary, after: &Summary) -> Result<(), ImportError> {
    require_eligible(after)?;
    if before.manifest_sha256 != after.manifest_sha256 {
        return Err(SourceChange::ExportMetadata.into());
    }
    if before.stat_observations_sha256 != after.stat_observations_sha256 {
        return Err(ImportFailure::SourceChanged {
            fields: vec![super::ChangedField::Inode, super::ChangedField::Ctime],
        }
        .into());
    }
    Ok(())
}

/// Imports an immutable live filesystem snapshot without loading server configuration.
pub fn execute(
    mut options: Options,
    mut output: impl Write,
    mut diagnostics: impl Write,
) -> Result<Summary, ImportError> {
    options.validate_destination(super::Destination::MayResume)?;
    options.verify_bodies = false;
    let source = fs::canonicalize(&options.source)?;
    let source = source
        .to_str()
        .ok_or_else(|| invalid("source root must be UTF-8"))?;
    let start =
        if fs::read_dir(&options.destination).is_ok_and(|mut entries| entries.next().is_some()) {
            ImportStart::Resume
        } else {
            ImportStart::Fresh
        };
    if start == ImportStart::Resume
        && !fs::symlink_metadata(options.destination.join("catalog.sqlite3"))
            .is_ok_and(|entry| entry.is_file())
    {
        return Err(invalid(
            "data-dir: choose an empty directory or the same unfinished import catalog",
        ));
    }
    let baseline = Baseline::create()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()?;
    let imported = runtime.block_on(run(
        &options,
        source_identity(&options, source),
        start,
        &baseline,
        &mut output,
        &mut diagnostics,
    ));
    // A refused resume changed nothing, so the destination can still be what an earlier release
    // started when it had other defaults.
    if start == ImportStart::Resume
        && matches!(imported, Err(ImportError::OtherImport))
        && let Some(former) = options.with_former_defaults()
    {
        return runtime.block_on(run(
            &former,
            source_identity(&former, source),
            start,
            &baseline,
            &mut output,
            &mut diagnostics,
        ));
    }
    imported
}

async fn run(
    options: &Options,
    source: [u8; 32],
    start: ImportStart,
    baseline: &File,
    output: &mut impl Write,
    diagnostics: &mut impl Write,
) -> Result<Summary, ImportError> {
    let preflight = if start == ImportStart::Resume {
        None
    } else {
        Some(metadata_scan(options, baseline, BaselineMode::Record, diagnostics).await?)
    };
    let catalog = if start == ImportStart::Resume {
        Catalog::resume_import(&options.destination, source)
            .await
            .map_err(resume_error)?
    } else {
        Catalog::open(&options.destination).await?
    };
    let result = async {
        let journal = catalog.import_journal();
        let original = journal.progress().await?;
        if start == ImportStart::Resume
            && !original
                .as_ref()
                .is_some_and(|state| state.is_open() && state.binding.source == source)
        {
            return Err(ImportError::OtherImport);
        }
        if let Some(failure) = original.as_ref().and_then(ImportProgress::failure) {
            return Err(ImportError::Resume(ResumeError::PermanentlyFailed {
                reason: failure.clone(),
            }));
        }
        let imported = async {
            let preflight = match preflight {
                Some(summary) => summary,
                None => metadata_scan(options, baseline, BaselineMode::Record, diagnostics).await?,
            };
            let fingerprint = preflight
                .manifest_sha256
                .as_deref()
                .ok_or_else(|| invalid("missing source fingerprint"))?;
            let binding = ImportBinding {
                source,
                fingerprint: sha(fingerprint.as_bytes()),
                expected_elements: i64::try_from(
                    preflight.eligible_buckets + preflight.eligible_objects,
                )?,
            };
            if original
                .as_ref()
                .is_some_and(|state| state.binding != binding)
            {
                return Err(SourceChange::Binding.into());
            }
            import(
                options,
                &catalog,
                &binding,
                &preflight,
                baseline,
                output,
                diagnostics,
            )
            .await
        }
        .await;
        if let Err(error) = imported {
            if terminal(&error)
                && let Some(state) = journal
                    .progress()
                    .await?
                    .filter(|state| state.is_open() && state.binding.source == source)
                && let Err(recording) = journal.fail(state.binding, error.to_string()).await
            {
                return Err(error.with_cleanup(CleanupOperation::TerminalFailure, recording));
            }
            return Err(error);
        }
        imported
    }
    .await;
    let closed = catalog.close().await;
    match (result, closed) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(original), Err(close)) => Err(original.with_cleanup(CleanupOperation::Catalog, close)),
    }
}

/// Names the refusal a resume reports. `Catalog::resume_import` carries the journal's refusal
/// inside a catalog failure, so this is where its type comes back.
fn resume_error(error: CatalogError) -> ImportError {
    if matches!(error, CatalogError::PreconditionFailed) {
        return ImportError::OtherImport;
    }
    let refusal = crate::error::causes(&error)
        .find_map(|cause| cause.downcast_ref::<ResumeError>())
        .and_then(|refusal| match refusal {
            ResumeError::PermanentlyFailed { reason } => Some(ResumeError::PermanentlyFailed {
                reason: reason.clone(),
            }),
            #[cfg(not(unix))]
            ResumeError::MissingLock => Some(ResumeError::MissingLock),
            // A catalog failure the journal met is reported as that failure.
            ResumeError::Catalog(_) => None,
        });
    refusal.map_or_else(|| error.into(), ImportError::Resume)
}

/// Whether a failure of the import itself is recorded in the journal, so that a resume refuses
/// the destination instead of repeating an attempt that cannot succeed.
fn terminal(error: &ImportError) -> bool {
    match error {
        ImportError::Admin(error) => match error {
            // A changed size or modification time means the content changed; stat identity
            // drift alone (device, inode or ctime) is retried.
            AdminError::SourceChanged { fields } => {
                fields.is_empty()
                    || fields.iter().any(|field| match field {
                        IdentityField::Device | IdentityField::Inode | IdentityField::Ctime => {
                            false
                        }
                        IdentityField::Size | IdentityField::Mtime => true,
                    })
            }
            AdminError::Checksum
            | AdminError::Corrupt(_)
            | AdminError::Invalid(_)
            | AdminError::Precondition
            | AdminError::Hold
            | AdminError::TooLarge
            | AdminError::NotFound => true,
            AdminError::Busy { .. }
            | AdminError::DiskFull(_)
            | AdminError::Closed
            | AdminError::SeedProjectMismatch
            | AdminError::SeedMetadataBudget { .. }
            | AdminError::Io(_)
            | AdminError::Forbidden
            | AdminError::BodyAborted(_)
            | AdminError::Backend(_) => false,
        },
        ImportError::Source(error) => {
            !error.retryable_stat_drift()
                && !matches!(error, ImportFailure::Io(_) | ImportFailure::ReportClosed)
        }
        // A native read that found malformed data cannot succeed on retry.
        ImportError::Io(error) => error.kind() == io::ErrorKind::InvalidData,
        ImportError::SourceChanged(_) | ImportError::Invalid(_) | ImportError::Codec(_) => true,
        // A refused resume changed nothing; it never reaches this check, which sees only the
        // failures of the import itself.
        ImportError::OtherImport | ImportError::Resume(_) => false,
        ImportError::SourceUnreadable
        | ImportError::Diagnostics(_)
        | ImportError::Catalog(_)
        | ImportError::Json(_)
        | ImportError::Integer(_)
        | ImportError::ObservationCleanup(_)
        | ImportError::Cleanup { .. } => false,
    }
}

async fn import(
    options: &Options,
    catalog: &Catalog,
    binding: &ImportBinding,
    preflight: &Summary,
    baseline: &File,
    output: &mut impl Write,
    diagnostics: &mut impl Write,
) -> Result<Summary, ImportError> {
    let journal = catalog.import_journal();
    let progress = journal.begin(binding.clone()).await?;
    let admin = catalog.admin(Origin::Import);
    let (mut receive, worker) = scan_with_baseline(
        options.clone(),
        crate::fsutil::read_source_metadata,
        Some((baseline.try_clone()?, BaselineMode::Verify)),
        super::ScanPurpose::Publish,
    )?;
    let mut bytes_hashed = 0;
    let mut body_digests = super::digest_context(b"bodies");
    let result = async {
        let mut ordinal = 0;
        let mut cursor = progress.cursor;
        let mut committed_page = VecDeque::new();
        while let Some(page) = receive.recv().await {
            findings(&page, diagnostics)?;
            for record in page.records {
                let Some(observed) = element(&record, ordinal + 1) else { continue };
                ordinal += 1;
                if let Record::Object(object) = &record {
                    let hashes = object.spec.hashes;
                    super::digest_fields(&mut body_digests, "body", &object.path, &[&hashes.md5, &hashes.crc32c.to_be_bytes()]);
                }
                if ordinal <= progress.cursor {
                    if committed_page.is_empty() {
                        committed_page = journal.page(binding.clone(), ordinal - 1, 256).await?.into();
                    }
                    let committed = committed_page.pop_front().filter(|element| element.ordinal == ordinal).ok_or_else(|| invalid("missing committed import observation"))?;
                    if committed.fingerprint != observed.fingerprint { return Err(SourceChange::CommittedIdentity.into()); }
                    verify_committed(catalog, &admin, &record).await?;
                    if committed.inode != observed.inode || committed.ctime_ns != observed.ctime_ns {
                        crate::diagnostics::Diagnostics {
                            info: Vec::new(),
                            warnings: vec![crate::diagnostics::Diagnostic::new(
                                crate::diagnostics::DiagnosticSource::Import,
                                format!("element.{ordinal}"),
                                "stat_identity_drift: revalidating this committed body's stored hashes before continuing; keep the source snapshot immutable.",
                            )],
                            errors: Vec::new(),
                        }.write(&mut *diagnostics)?;
                        if let Record::Object(object) = record {
                            let staged = admin.stage_from_file(object.source_file.ok_or_else(|| invalid("missing opened source body"))?, object.spec.hashes.into(), options.max_object_bytes).await?;
                            bytes_hashed += staged.size();
                            staged.discard().await?;
                        }
                        journal.revalidate(binding.clone(), committed, observed).await?;
                    }
                    continue;
                }
                bytes_hashed += publish(&admin, binding, cursor, observed, record, options.max_object_bytes).await?;
                cursor = ordinal;
                serde_json::to_writer(&mut *output, &serde_json::json!({"event":"import_progress","cursor":cursor,"expected":binding.expected_elements}))?;
                writeln!(output)?;
            }
        }
        Ok::<_, ImportError>(())
    }.await;
    drop(receive);
    let joined = join_scan(worker).await?;
    result?;
    let published = joined?;
    same_export(preflight, &published)?;
    let final_scan = metadata_scan(options, baseline, BaselineMode::Verify, diagnostics).await?;
    same_export(preflight, &final_scan)?;
    journal.finish(binding.clone()).await?;
    let summary = Summary {
        bodies_verified: true,
        bytes_hashed,
        body_digests_sha256: Some(super::finish_digest(body_digests)),
        ..final_scan
    };
    serde_json::to_writer(
        &mut *output,
        &serde_json::json!({"event":"import_complete","summary":summary,"preservation_scope": match options.source_format {
            super::SourceFormat::Filesystem => "recoverable current live disk state; generated native ETags, metageneration and bucket defaults; no history or IAM",
            super::SourceFormat::Snapshot => "quiesced memory snapshot with explicit live/archive membership, source identities and metadata; generated native ETags, metageneration, project and retention enablement; no IAM",
        }}),
    )?;
    writeln!(output)?;
    Ok(summary)
}

async fn join_scan(
    worker: super::ScanWorker,
) -> Result<Result<Summary, ImportFailure>, ImportError> {
    worker.await.map_err(|cause| {
        io::Error::other(crate::storage::failure::BackendCause::with_source(
            "import scanner stopped without a result",
            cause,
        ))
        .into()
    })
}

async fn publish(
    admin: &Admin,
    binding: &ImportBinding,
    cursor: i64,
    element: ImportElement,
    record: Record,
    cap: u64,
) -> Result<u64, ImportError> {
    match record {
        Record::Bucket(bucket) => {
            publish_bucket(admin, binding, cursor, element, *bucket).await?;
            Ok(0)
        }
        Record::Object(object) => {
            publish_object(admin, binding, cursor, element, *object, cap).await
        }
        Record::Directory { .. } => {
            Err(invalid("directories cannot be published as source bodies"))
        }
    }
}

async fn publish_bucket(
    admin: &Admin,
    binding: &ImportBinding,
    cursor: i64,
    element: ImportElement,
    record: BucketRecord,
) -> Result<(), ImportError> {
    let BucketRecord {
        name,
        attributes,
        enable_object_retention,
        spec,
        ..
    } = record;
    let mut input = serde_json::Map::from_iter([
        ("name".into(), serde_json::json!(name)),
        (
            "defaultEventBasedHold".into(),
            serde_json::json!(attributes.default_event_based_hold),
        ),
    ]);
    if let Some(enabled) = attributes.versioning_enabled {
        input.insert("versioning".into(), serde_json::json!({"enabled":enabled}));
    }
    let input = serde_json::Value::Object(input).to_string();
    let mut bucket =
        crate::json::parse_insert(input.as_bytes(), "local-project", SystemTime::now())
            .map_err(|error| invalid(error.message))?;
    if let Some(created) = spec.created {
        bucket.time_created = created;
        bucket.updated = created;
    }
    bucket.object_retention_enabled = enable_object_retention;
    let definition = BucketDefinition::new(bucket)?;
    admin
        .publish_import_bucket_with_acl(
            binding.clone(),
            cursor,
            element,
            definition,
            spec.acl,
            spec.created,
        )
        .await?;
    Ok(())
}

/// Publishes one source object; returns the bytes it hashed.
async fn publish_object(
    admin: &Admin,
    binding: &ImportBinding,
    cursor: i64,
    element: ImportElement,
    record: ObjectRecord,
    cap: u64,
) -> Result<u64, ImportError> {
    let identity = record.identity()?;
    let ObjectRecord {
        bucket,
        name,
        attributes,
        source_file,
        spec,
        ..
    } = record;
    let request = spec.into_publication(attributes, cap);
    let staged = admin
        .stage_from_file(
            source_file.ok_or_else(|| invalid("missing opened source body"))?,
            request.expected,
            request.max_bytes,
        )
        .await?;
    let bytes = staged.size();
    let mut batch = admin
        .publish_import_objects(
            binding.clone(),
            cursor,
            vec![crate::storage::admin::ImportedPublication {
                publication: BatchPublication {
                    bucket,
                    name,
                    request,
                    staged,
                },
                identity,
                element,
            }],
        )
        .await?;
    if !batch.unattempted.is_empty() {
        for entry in batch.unattempted {
            entry.publication.staged.discard().await?;
        }
        return Err(AdminError::busy().into());
    }
    batch.results.pop().ok_or_else(|| {
        invalid("import publication returned no result; inspect journal before retrying")
    })??;
    Ok(bytes)
}

async fn verify_committed(
    catalog: &Catalog,
    admin: &Admin,
    record: &Record,
) -> Result<(), ImportError> {
    match record {
        Record::Bucket(bucket) => verify_bucket(catalog, admin, bucket).await,
        Record::Object(object) => verify_object(catalog, admin, object).await,
        Record::Directory { .. } => Ok(()),
    }
}

async fn verify_bucket(
    catalog: &Catalog,
    admin: &Admin,
    record: &BucketRecord,
) -> Result<(), ImportError> {
    let BucketRecord {
        name,
        attributes,
        enable_object_retention,
        spec,
        ..
    } = record;
    let target = admin
        .get_bucket(name)
        .await?
        .ok_or_else(|| invalid("missing committed bucket"))?;
    if target.default_event_based_hold != attributes.default_event_based_hold
        || target.project != "local-project"
        || target.versioning != crate::bucket::Versioning::from(attributes.versioning_enabled)
        || target.object_retention_enabled != *enable_object_retention
        || spec
            .created
            .is_some_and(|created| created != target.time_created || created != target.updated)
    {
        return Err(invalid("committed bucket differs from the source binding"));
    }
    if let Some(expected) = &spec.acl {
        let projection = catalog
            .acl_projection(name.as_str(), Vec::new())
            .await?
            .ok_or_else(|| invalid("missing committed bucket ACL"))?;
        if projection.bucket != *expected {
            return Err(invalid(
                "committed bucket ACL differs from the source binding",
            ));
        }
    }
    Ok(())
}

async fn verify_object(
    catalog: &Catalog,
    admin: &Admin,
    record: &ObjectRecord,
) -> Result<(), ImportError> {
    let ObjectRecord {
        bucket,
        name,
        attributes,
        created_micros,
        updated_micros,
        deleted_micros,
        fingerprint,
        spec,
        ..
    } = record;
    let generation = spec.generation.get();
    let target = admin
        .get_object(bucket, name, Some(spec.generation))
        .await?;
    if deleted_micros.is_none()
        && admin.get_object(bucket, name, None).await?.generation != generation
    {
        return Err(invalid(
            "committed live membership differs from the source binding",
        ));
    }
    if let Some(expected) = &spec.acl {
        let projection = catalog
            .acl_projection(
                bucket.as_str(),
                vec![(name.as_str().to_owned(), generation)],
            )
            .await?
            .ok_or_else(|| invalid("missing committed object ACL"))?;
        if projection
            .objects
            .first()
            .map(crate::storage::ObjectAcl::entries)
            != Some(expected.as_slice())
        {
            return Err(invalid(
                "committed object ACL differs from the source binding",
            ));
        }
    }
    if target.generation != generation
        || target.time_created != time(*created_micros)?
        || target.updated != time(*updated_micros)?
        || target.time_deleted != deleted_micros.map(time).transpose()?
        || target.retention != spec.retention
        || target.md5 != spec.hashes.md5
        || target.crc32c != spec.hashes.crc32c
        || target.size != fingerprint.size
        || target.metageneration != 1
        || target.content_type != attributes.content_type
        || target.content_encoding.as_deref().unwrap_or_default() != attributes.content_encoding
        || target.content_disposition.as_deref().unwrap_or_default()
            != attributes.content_disposition
        || target.content_language.as_deref().unwrap_or_default() != attributes.content_language
        || target.cache_control.as_deref().unwrap_or_default() != attributes.cache_control
        || target.metadata != attributes.metadata.clone().unwrap_or_default()
        || target.event_based_hold.is_held()
        || target.storage_class != spec.storage_class
        || target.custom_time != spec.custom_time
    {
        return Err(invalid("committed object differs from the source binding"));
    }
    Ok(())
}

#[cfg(test)]
mod error_tests {
    use super::*;

    #[test]
    fn terminal_classification_preserves_retryable_identity_drift_and_io_failures() {
        for (error, expected) in [
            (AdminError::Hold.into(), true),
            (AdminError::Invalid("invalid metadata".into()).into(), true),
            (AdminError::busy().into(), false),
            (
                AdminError::SourceChanged {
                    fields: vec![IdentityField::Inode, IdentityField::Ctime],
                }
                .into(),
                false,
            ),
            (
                AdminError::SourceChanged {
                    fields: vec![IdentityField::Size],
                }
                .into(),
                true,
            ),
            (
                ImportFailure::SourceChanged {
                    fields: vec![super::super::ChangedField::Inode],
                }
                .into(),
                false,
            ),
            (
                ImportFailure::SourceChanged {
                    fields: vec![super::super::ChangedField::Metadata],
                }
                .into(),
                true,
            ),
            (io::Error::from(io::ErrorKind::InvalidData).into(), true),
            (
                io::Error::from(io::ErrorKind::PermissionDenied).into(),
                false,
            ),
            (SourceChange::ExportMetadata.into(), true),
            (SourceChange::Binding.into(), true),
            (SourceChange::CommittedIdentity.into(), true),
            (
                invalid("committed bucket differs from the source binding"),
                true,
            ),
            (ImportError::SourceUnreadable, false),
            (codec::TimestampError::OUTSIDE_PLATFORM_RANGE.into(), true),
        ] {
            assert_eq!(terminal(&error), expected, "{error}");
        }
    }
}
