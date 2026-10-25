#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{ImportFailure, Options, Page, Record, Summary, codec, scan_with_baseline};
use crate::{
    object::{ExpectedHashes, ObjectMetadataUpdate},
    storage::{
        Catalog,
        admin::{
            Admin, AdminError, BatchPublication, BucketDefinition, BucketName, Generation,
            GenerationCondition, ImportedIdentity, ObjectName, Origin, Publication, PublishMode,
        },
        import_journal::{ImportBinding, ImportElement},
    },
};
use ring::digest::{Context, SHA256, digest};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Failure = Box<dyn std::error::Error>;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
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
        Record::Bucket {
            path,
            name,
            attributes_digest,
            ..
        } => {
            super::digest_fields(
                &mut context,
                "bucket",
                path,
                &[name.as_bytes(), attributes_digest],
            );
            (None, None)
        }
        Record::Object {
            path,
            bucket,
            name,
            attributes_digest,
            fingerprint,
            ..
        } => {
            super::digest_fields(
                &mut context,
                "object",
                path,
                &[
                    bucket.as_bytes(),
                    name.as_bytes(),
                    attributes_digest,
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

/// This execution's disk-backed observations are never resume authority.
pub(super) struct Baseline {
    file: File,
    write: bool,
}

impl Baseline {
    pub(super) fn open(path: &Path, write: bool) -> io::Result<Self> {
        let file = if write {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path)?
        } else {
            File::open(path)?
        };
        Ok(Self { file, write })
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
        if let Record::Object { fingerprint, .. } = record {
            bytes[58..].copy_from_slice(&fingerprint.device.unwrap_or_default().to_be_bytes());
        }
        if self.write {
            self.file.write_all(&bytes)?;
        } else {
            let mut expected = [0; 66];
            if let Err(error) = self.file.read_exact(&mut expected) {
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
        Ok(())
    }
}

fn time(micros: i64) -> Result<SystemTime, io::Error> {
    let duration = Duration::from_micros(micros.unsigned_abs());
    if micros >= 0 {
        UNIX_EPOCH.checked_add(duration)
    } else {
        UNIX_EPOCH.checked_sub(duration)
    }
    .ok_or_else(|| invalid("source timestamp is outside this platform's range"))
}

fn publication(attrs: &codec::ObjectAttrs, cap: u64) -> Result<Publication, Failure> {
    if attrs.storage_class != "STANDARD" {
        return Err(invalid("StorageClass: file import currently preserves only STANDARD; use a compatible export adapter").into());
    }
    Ok(Publication {
        content_type: attrs.content_type.clone(),
        metadata: ObjectMetadataUpdate {
            content_encoding: Some(Some(attrs.content_encoding.clone())),
            content_disposition: Some(Some(attrs.content_disposition.clone())),
            content_language: Some(Some(attrs.content_language.clone())),
            cache_control: Some(Some(attrs.cache_control.clone())),
            metadata: Some(attrs.metadata.clone().map(|entries| {
                entries
                    .into_iter()
                    .map(|(key, value)| (key, Some(value)))
                    .collect()
            })),
            event_based_hold: Some(false),
            ..Default::default()
        },
        expected: ExpectedHashes {
            md5: Some(
                crate::json::decode_hash::<16>(&attrs.md5_hash)
                    .map_err(|_| invalid("invalid source MD5"))?,
            ),
            crc32c: Some(u32::from_be_bytes(
                crate::json::decode_hash::<4>(&attrs.crc32c)
                    .map_err(|_| invalid("invalid source CRC32C"))?,
            )),
        },
        condition: Some(GenerationCondition::Absent),
        max_bytes: cap,
        mode: PublishMode::Replace,
    })
}

fn findings(page: &Page, diagnostics: &mut impl Write) -> Result<(), Failure> {
    let mut batch = crate::diagnostics::Diagnostics::default();
    for finding in &page.findings {
        if finding.blocking {
            batch.errors.push(finding.diagnostic.clone());
        } else {
            batch.warnings.push(finding.diagnostic.clone());
        }
    }
    batch.write(diagnostics)?;
    Ok(())
}

async fn metadata_scan(
    options: &Options,
    baseline: &Path,
    write: bool,
    diagnostics: &mut impl Write,
) -> Result<Summary, Failure> {
    let (mut receive, worker) = scan_with_baseline(
        options.clone(),
        crate::fsutil::read_source_metadata,
        Some((baseline.into(), write)),
        false,
    )?;
    let result = async {
        while let Some(page) = receive.recv().await {
            findings(&page, diagnostics)?;
            for record in page.records {
                if let Record::Object {
                    attributes,
                    created_micros,
                    updated_micros,
                    ..
                } = record
                {
                    publication(&attributes, options.max_object_bytes)?;
                    ImportedIdentity::new(
                        Generation::new(attributes.generation)?,
                        time(created_micros)?,
                        time(updated_micros)?,
                    )?;
                }
            }
        }
        Ok::<_, Failure>(())
    }
    .await;
    drop(receive);
    let joined = worker
        .join()
        .map_err(|_| invalid("import scanner panicked"))?;
    result?;
    let summary = joined?;
    require_eligible(&summary)?;
    Ok(summary)
}

fn require_eligible(summary: &Summary) -> Result<(), Failure> {
    if !summary.eligible {
        if summary.io_findings != 0
            && summary.io_findings + summary.stat_identity_drift_findings
                == summary.blocking_findings
        {
            return Err(io::Error::other("source I/O failure: resolve the reported filesystem error and retry the same immutable snapshot").into());
        }
        if summary.blocking_findings != 0
            && summary.stat_identity_drift_findings == summary.blocking_findings
        {
            return Err(ImportFailure::SourceChanged {
                fields: vec![super::ChangedField::Inode, super::ChangedField::Ctime],
            }
            .into());
        }
        return Err(invalid(
            "import preflight found blockers; restore an immutable supported snapshot and retry",
        )
        .into());
    }
    Ok(())
}

fn same_export(before: &Summary, after: &Summary) -> Result<(), Failure> {
    require_eligible(after)?;
    if before.manifest_sha256 != after.manifest_sha256 {
        return Err(invalid("source_changed: keep the source immutable; abandon this incomplete destination if export metadata changed").into());
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
) -> Result<Summary, Failure> {
    options.validate_destination(true)?;
    options.verify_bodies = false;
    let source = fs::canonicalize(&options.source)?;
    let source = source
        .to_str()
        .ok_or_else(|| invalid("source root must be UTF-8"))?;
    let mut source_context = Context::new(&SHA256);
    source_context.update(b"gcs-mimic-import-binding\0\x01");
    super::digest_fields(
        &mut source_context,
        "source",
        source,
        &[
            super::SOURCE_REVISION.as_bytes(),
            &[super::FINGERPRINT_FORMAT],
            &[match options.metadata_mode {
                super::MetadataMode::Xattr => 0,
                super::MetadataMode::Sidecar => 1,
            }],
            &[match options.source_platform {
                super::SourcePlatform::Unix => 0,
                super::SourcePlatform::Windows => 1,
            }],
            &options.max_object_bytes.to_be_bytes(),
            &(options.max_entries as u64).to_be_bytes(),
            &(options.max_depth as u64).to_be_bytes(),
        ],
    );
    let mut source_id = [0; 32];
    source_id.copy_from_slice(source_context.finish().as_ref());
    let resume =
        fs::read_dir(&options.destination).is_ok_and(|mut entries| entries.next().is_some());
    if resume
        && !fs::symlink_metadata(options.destination.join("catalog.sqlite3"))
            .is_ok_and(|entry| entry.is_file())
    {
        return Err(invalid(
            "data-dir: choose an empty directory or the same unfinished import catalog",
        )
        .into());
    }
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    let baseline = std::env::temp_dir().join(format!(
        "gcs-mimic-import-{}",
        crate::hashing::encode_lower(&random)
    ));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(
        &options,
        source_id,
        resume,
        &baseline,
        &mut output,
        &mut diagnostics,
    ));
    match fs::remove_file(&baseline) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => {
            return Err(io::Error::other(match result {
                Ok(_) => format!("remove temporary import observations: {error}"),
                Err(original) => {
                    format!("{original}; remove temporary import observations also failed: {error}")
                }
            })
            .into());
        }
    }
    result
}

async fn run(
    options: &Options,
    source: [u8; 32],
    resume: bool,
    baseline: &Path,
    output: &mut impl Write,
    diagnostics: &mut impl Write,
) -> Result<Summary, Failure> {
    let preflight = if resume {
        None
    } else {
        Some(metadata_scan(options, baseline, true, diagnostics).await?)
    };
    let catalog = Catalog::open(&options.destination).await?;
    let result = async {
        let journal = catalog.import_journal();
        let original = journal.progress().await?;
        if resume && !original.as_ref().is_some_and(|state| state.active && state.binding.source == source) {
            return Err(invalid("data-dir: destination is not the same unfinished import with matching options").into());
        }
        if let Some(failure) = original.as_ref().and_then(|state| state.failure.as_ref()) {
            return Err(invalid(format!("import permanently failed: {failure}; preserve the source snapshot and restart into a new empty destination")).into());
        }
        let imported = async {
            let preflight = match preflight {
                Some(summary) => summary,
                None => metadata_scan(options, baseline, true, diagnostics).await?,
            };
            let fingerprint = preflight.manifest_sha256.as_deref().ok_or_else(|| invalid("missing source fingerprint"))?;
            let binding = ImportBinding {
                source,
                fingerprint: sha(fingerprint.as_bytes()),
                expected_elements: i64::try_from(preflight.eligible_buckets + preflight.eligible_objects)?,
            };
            if original.as_ref().is_some_and(|state| state.binding != binding) {
                return Err(invalid("source_changed: abandon the incomplete destination and use a fresh immutable export").into());
            }
            import(options, &catalog, &binding, &preflight, baseline, output, diagnostics).await
        }.await;
        if let Err(error) = &imported
            && terminal(error.as_ref())
            && let Some(state) = journal.progress().await?.filter(|state| state.active && state.binding.source == source)
        {
            journal.fail(state.binding, error.to_string()).await.map_err(|recording| {
                io::Error::other(format!("{error}; persist terminal import failure also failed: {recording}"))
            })?;
        }
        imported
    }.await;
    let closed = catalog.close().await;
    match (result, closed) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(error.into()),
        (Err(original), Err(close)) => {
            Err(io::Error::other(format!("{original}; catalog close also failed: {close}")).into())
        }
    }
}

fn terminal(error: &(dyn std::error::Error + 'static)) -> bool {
    if let Some(error) = error.downcast_ref::<AdminError>() {
        if let AdminError::SourceChanged { fields } = error {
            return fields.is_empty()
                || fields
                    .iter()
                    .any(|field| !matches!(field.as_str(), "device" | "inode" | "ctime_ns"));
        }
        return matches!(
            error,
            AdminError::Checksum
                | AdminError::Corrupt(_)
                | AdminError::Invalid(_)
                | AdminError::Precondition
                | AdminError::Hold
                | AdminError::TooLarge
                | AdminError::NotFound
        );
    }
    if let Some(error) = error.downcast_ref::<ImportFailure>() {
        return !error.retryable_stat_drift()
            && !matches!(error, ImportFailure::Io(_) | ImportFailure::ReportClosed);
    }
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::InvalidData)
}

async fn import(
    options: &Options,
    catalog: &Catalog,
    binding: &ImportBinding,
    preflight: &Summary,
    baseline: &Path,
    output: &mut impl Write,
    diagnostics: &mut impl Write,
) -> Result<Summary, Failure> {
    let journal = catalog.import_journal();
    let progress = journal.begin(binding.clone()).await?;
    let admin = catalog.admin(Origin::Import);
    let (mut receive, worker) = scan_with_baseline(
        options.clone(),
        crate::fsutil::read_source_metadata,
        Some((baseline.into(), false)),
        true,
    )?;
    let mut bytes_hashed = 0;
    let mut body_digests = super::digest_context(b"bodies");
    let result = async {
        let mut ordinal = 0;
        let mut cursor = progress.cursor;
        while let Some(page) = receive.recv().await {
            findings(&page, diagnostics)?;
            for record in page.records {
                let Some(observed) = element(&record, ordinal + 1) else { continue };
                ordinal += 1;
                if let Record::Object { path, attributes, .. } = &record {
                    let expected = publication(attributes, options.max_object_bytes)?.expected;
                    if let (Some(md5), Some(crc)) = (expected.md5, expected.crc32c) {
                        super::digest_fields(&mut body_digests, "body", path, &[&md5, &crc.to_be_bytes()]);
                    }
                }
                if ordinal <= progress.cursor {
                    let committed = journal.page(binding.clone(), ordinal - 1, 1).await?.into_iter().next().ok_or_else(|| invalid("missing committed import observation"))?;
                    if committed.fingerprint != observed.fingerprint { return Err(invalid("source_changed: committed source identity differs").into()); }
                    verify_committed(&admin, &record).await?;
                    if committed.inode != observed.inode || committed.ctime_ns != observed.ctime_ns {
                        crate::diagnostics::Diagnostics {
                            warnings: vec![crate::diagnostics::Diagnostic::new(
                                crate::diagnostics::DiagnosticSource::Import,
                                format!("element.{ordinal}"),
                                "stat_identity_drift: revalidating this committed body's stored hashes before continuing; keep the source snapshot immutable.",
                            )],
                            errors: Vec::new(),
                        }.write(&mut *diagnostics)?;
                        if let Record::Object { attributes, source_file, .. } = record {
                            let request = publication(&attributes, options.max_object_bytes)?;
                            let staged = admin.stage_from_file(source_file.ok_or_else(|| invalid("missing opened source body"))?, request.expected, request.max_bytes).await?;
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
        Ok::<_, Failure>(())
    }.await;
    drop(receive);
    let joined = worker
        .join()
        .map_err(|_| invalid("import scanner panicked"))?;
    result?;
    let published = joined?;
    same_export(preflight, &published)?;
    let final_scan = metadata_scan(options, baseline, false, diagnostics).await?;
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
        &serde_json::json!({"event":"import_complete","summary":summary,"preservation_scope":"recoverable current live disk state; generated native ETags, metageneration and bucket defaults; no history or IAM"}),
    )?;
    writeln!(output)?;
    Ok(summary)
}

async fn publish(
    admin: &Admin,
    binding: &ImportBinding,
    cursor: i64,
    element: ImportElement,
    record: Record,
    cap: u64,
) -> Result<u64, Failure> {
    match record {
        Record::Bucket {
            name, attributes, ..
        } => {
            let input = serde_json::json!({"name":name,"defaultEventBasedHold":attributes.default_event_based_hold}).to_string();
            let definition = BucketDefinition::new(
                crate::json::parse_insert(input.as_bytes(), "local-project", SystemTime::now())
                    .map_err(|error| invalid(error.message))?,
            )?;
            admin
                .publish_import_bucket(binding.clone(), cursor, element, definition)
                .await?;
            Ok(0)
        }
        Record::Object {
            bucket,
            name,
            attributes,
            created_micros,
            updated_micros,
            source_file,
            ..
        } => {
            let request = publication(&attributes, cap)?;
            let identity = ImportedIdentity::new(
                Generation::new(attributes.generation)?,
                time(created_micros)?,
                time(updated_micros)?,
            )?;
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
                            bucket: BucketName::new(bucket)?,
                            name: ObjectName::new(name)?,
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
                return Err(AdminError::Busy {
                    retry_after: Duration::from_secs(1),
                }
                .into());
            }
            batch.results.pop().ok_or_else(|| {
                invalid("import publication returned no result; inspect journal before retrying")
            })??;
            Ok(bytes)
        }
        Record::Directory { .. } => {
            Err(invalid("directories cannot be published as source bodies").into())
        }
    }
}

async fn verify_committed(admin: &Admin, record: &Record) -> Result<(), Failure> {
    match record {
        Record::Bucket {
            name, attributes, ..
        } => {
            let target = admin
                .get_bucket(&BucketName::new(name.clone())?)
                .await?
                .ok_or_else(|| invalid("missing committed bucket"))?;
            if target.default_event_based_hold != attributes.default_event_based_hold
                || target.project != "local-project"
                || target.versioning_enabled
            {
                return Err(invalid("committed bucket differs from the source binding").into());
            }
        }
        Record::Object {
            bucket,
            name,
            attributes,
            created_micros,
            updated_micros,
            fingerprint,
            ..
        } => {
            let target = admin
                .get_object(
                    &BucketName::new(bucket.clone())?,
                    &ObjectName::new(name.clone())?,
                    None,
                )
                .await?;
            let request = publication(attributes, fingerprint.size)?;
            if target.generation != attributes.generation
                || target.time_created != time(*created_micros)?
                || target.updated != time(*updated_micros)?
                || Some(target.md5) != request.expected.md5
                || Some(target.crc32c) != request.expected.crc32c
                || target.size != fingerprint.size
                || target.metageneration != 1
                || target.content_type != attributes.content_type
                || target.content_encoding.as_deref().unwrap_or_default()
                    != attributes.content_encoding
                || target.content_disposition.as_deref().unwrap_or_default()
                    != attributes.content_disposition
                || target.content_language.as_deref().unwrap_or_default()
                    != attributes.content_language
                || target.cache_control.as_deref().unwrap_or_default() != attributes.cache_control
                || target.metadata != attributes.metadata.clone().unwrap_or_default()
                || target.event_based_hold
                || target.storage_class != attributes.storage_class
            {
                return Err(invalid("committed object differs from the source binding").into());
            }
        }
        Record::Directory { .. } => (),
    }
    Ok(())
}
