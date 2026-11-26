#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Read-only inspection of pinned fake-gcs-server durable filesystem snapshots.

pub(crate) mod codec;
mod error;
mod filesystem;
mod reader;
mod snapshot;
mod spec;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

use crate::{
    bucket::name::BucketName,
    config::{MAX_OBJECT_BYTES, MAX_TREE_DEPTH},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    hashing::Hashes,
    object::name::ObjectName,
    source_tree,
};
pub use codec::CodecError;
pub use error::{CleanupOperation, ImportError, SourceChange};
pub use filesystem::execute;
pub use reader::{BodyStream, ChangedField, CorruptField, Fingerprint, ImportFailure};
use ring::digest::{Context, SHA256};
use serde::Serialize;
use spec::{BucketSpec, ObjectSpec};
use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{self, Write},
    num::NonZeroUsize,
    path::PathBuf,
    str::FromStr,
};
use tokio::sync::mpsc;

pub(crate) const SOURCE_REVISION: &str = "896ece51e49eed0946089f3089f6343855c0d106";
pub(crate) const PAGE_SIZE: usize = 256;
const PUBLICATION_PAGE_SIZE: usize = 8;
pub(crate) const FINGERPRINT_FORMAT: u8 = 1;
/// The limits an earlier release recorded in the identity of an import that named none.
const FORMER_DEFAULT_OBJECT_BYTES: u64 = 1_073_741_824;
const FORMER_DEFAULT_ENTRIES: Option<NonZeroUsize> = NonZeroUsize::new(10_000);
const FORMER_DEFAULT_DEPTH: Option<NonZeroUsize> = NonZeroUsize::new(32);
pub(crate) const HELP: &str = "Usage: gcs-mimic import fake-gcs-server --source DIR --data-dir NEW
       --metadata-mode xattr|sidecar --source-platform unix|windows [--dry-run [--verify-bodies]]
       [--max-object-bytes N] [--max-entries N] [--max-depth N]
       gcs-mimic import fake-gcs-server --source DIR --data-dir NEW --source-format snapshot
       [--max-object-bytes N] [--max-entries N]

Import a stopped writer's durable filesystem snapshot without changing the source.
NEW must be absent or empty, or contain the same unfinished import for automatic resume.
--dry-run inspects metadata without creating NEW; --verify-bodies also verifies its bodies.
Actual import always verifies body checksums and keeps unfinished destinations unservable.
Output is JSON Lines; diagnostics use stderr. Source and destination must be separate.
Filesystem snapshots recover normalized disk names only; original names/history cannot be verified.
Native project/default placement and ETags are generated. Snapshot format preserves declared memory history.
No server configuration is read.

  --max-object-bytes N  5497558138880 (0..5497558138880; largest source body; 0 is the largest)
  --max-entries N       0 (source files and directories, or snapshot rows; 0 is unlimited)
  --max-depth N         0 (0..1024; directory levels below a bucket; filesystem format only; 0 is unlimited)
These limits identify an import: a resume must repeat the values of its first run.
An import that an earlier release started without them also resumes without them.";

crate::choice::string_enum! {
    no_default
    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum MetadataMode {
        Xattr => "xattr",
        Sidecar => "sidecar",
    }
    "Use xattr or sidecar, matching the entire source root."
}
crate::choice::string_enum! {
    no_default
    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum SourcePlatform {
        Unix => "unix",
        Windows => "windows",
    }
    "Use unix or windows, matching the source path platform."
}

crate::choice::string_enum! {
    #[derive(Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum SourceFormat {
        #[default]
        Filesystem => "filesystem",
        Snapshot => "snapshot",
    }
    "Use filesystem or snapshot."
}

/// Whether the destination must be empty (inspection) or may hold an unfinished import to
/// resume (actual import).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Destination {
    MustBeEmpty,
    MayResume,
}

/// Why a scan runs: inspection only reports, while publication keeps each source body open
/// and uses smaller pages so few descriptors are held at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanPurpose {
    Inspect,
    Publish,
}

/// Whether a finding blocks the import or is only a warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Severity {
    Blocking,
    Warning,
}

/// A check that reported a blocking finding instead of producing its value. The finding is
/// already in the report; the marker only keeps the entry out of publication. Whether an entry
/// is published follows from its own checks, never from the scan-wide counter, which every
/// other entry raises too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reported;

/// A value parsed from source attributes, or the blocking finding reported in its place.
pub(crate) type Checked<T> = Result<T, Reported>;

#[derive(Clone, Debug)]
pub struct Options {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub metadata_mode: MetadataMode,
    pub source_platform: SourcePlatform,
    pub source_format: SourceFormat,
    pub verify_bodies: bool,
    pub max_object_bytes: u64,
    pub max_entries: Option<NonZeroUsize>,
    pub max_depth: Option<NonZeroUsize>,
}

impl Options {
    pub fn new(
        source: PathBuf,
        destination: PathBuf,
        metadata_mode: MetadataMode,
        source_platform: SourcePlatform,
    ) -> Self {
        Self {
            source,
            destination,
            metadata_mode,
            source_platform,
            source_format: SourceFormat::Filesystem,
            verify_bodies: false,
            max_object_bytes: MAX_OBJECT_BYTES,
            max_entries: None,
            max_depth: None,
        }
    }

    /// These options as an earlier release ran them when they named no limit. Limits are part of
    /// an import's identity, and that release recorded its own defaults: 1 GiB, 10,000 entries and
    /// 32 levels. Only options that name no limit themselves can mean them.
    fn with_former_defaults(&self) -> Option<Self> {
        (self.max_object_bytes == MAX_OBJECT_BYTES
            && self.max_entries.is_none()
            && self.max_depth.is_none())
        .then(|| Self {
            max_object_bytes: FORMER_DEFAULT_OBJECT_BYTES,
            max_entries: FORMER_DEFAULT_ENTRIES,
            max_depth: FORMER_DEFAULT_DEPTH,
            ..self.clone()
        })
    }

    fn tree_limits(&self) -> source_tree::TreeLimits {
        source_tree::TreeLimits {
            entries: self.max_entries,
            depth: self.max_depth,
        }
    }

    fn validate(&self) -> Result<(), Diagnostics> {
        self.validate_destination(Destination::MustBeEmpty)
    }

    fn validate_destination(&self, destination: Destination) -> Result<(), Diagnostics> {
        let mut diagnostics = Diagnostics::default();
        let mut issue = |key, fix| {
            diagnostics
                .errors
                .push(Diagnostic::new(DiagnosticSource::Import, key, fix))
        };
        if !fs::symlink_metadata(&self.source).is_ok_and(|m| m.is_dir()) {
            issue(
                "--source",
                "Supply an accessible directory, not a symlink, from a stopped writer or immutable snapshot.",
            );
        }
        match fs::symlink_metadata(&self.destination) {
            Ok(m) if m.is_dir() => {
                if destination == Destination::MustBeEmpty
                    && !fs::read_dir(&self.destination)
                        .is_ok_and(|mut entries| entries.next().is_none())
                {
                    issue(
                        "--data-dir",
                        "Choose a separate empty destination directory, for example converted-data.",
                    );
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            _ => issue(
                "--data-dir",
                "Choose an absent or empty directory, not a symlink, for example converted-data.",
            ),
        }
        match source_tree::paths_overlap(&self.source, &self.destination) {
            Ok(false) => (),
            Ok(true) => issue(
                "--data-dir",
                "Source and destination must not overlap in either direction; choose a separate directory.",
            ),
            Err(_) => issue(
                "--data-dir",
                "Source and destination parent must exist and be accessible; create only the destination parent before retrying.",
            ),
        }
        if self.source_format == SourceFormat::Filesystem
            && self.metadata_mode == MetadataMode::Xattr
            && self.source_platform == SourcePlatform::Windows
        {
            issue(
                "--metadata-mode",
                "Windows source snapshots use sidecars; specify --metadata-mode sidecar.",
            );
        }
        // Snapshot reads pin the capture through descriptor-relative opens, which exist only on
        // Unix; reject up front instead of reporting a generic source I/O failure.
        if self.source_format == SourceFormat::Snapshot && cfg!(not(unix)) {
            issue(
                "--source-format",
                "Memory snapshot import is supported on Linux and macOS only; import the capture on one of those hosts.",
            );
        }
        if self
            .max_depth
            .is_some_and(|depth| depth.get() > MAX_TREE_DEPTH)
        {
            issue("limits", "Use at most 1024 directory levels.");
        }
        if diagnostics.is_err() {
            Err(diagnostics)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct Finding {
    pub diagnostic: Diagnostic,
    pub reason: String,
    pub blocking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_fields: Vec<ChangedField>,
}

/// One eligible source entry. The dry-run report shows each as its `kind` tag followed by the
/// record's fields in order; the typed specs stay out of it.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Record {
    Bucket(Box<BucketRecord>),
    Object(Box<ObjectRecord>),
    Directory { path: String },
}

#[derive(Debug, Serialize)]
pub(crate) struct BucketRecord {
    path: String,
    name: BucketName,
    attributes: codec::BucketAttrs,
    created_timestamp: Option<String>,
    enable_object_retention: bool,
    generated: &'static [&'static str],
    observed_stat: String,
    #[serde(skip)]
    attributes_digest: [u8; 32],
    #[serde(skip)]
    spec: BucketSpec,
}

#[derive(Debug, Serialize)]
pub(crate) struct ObjectRecord {
    path: String,
    bucket: BucketName,
    name: ObjectName,
    attributes: codec::ObjectAttrs,
    created_micros: i64,
    updated_micros: i64,
    deleted_micros: Option<i64>,
    fingerprint: Fingerprint,
    verified_hashes: Option<Hashes>,
    generated: &'static [&'static str],
    #[serde(skip)]
    source_file: Option<fs::File>,
    #[serde(skip)]
    attributes_digest: [u8; 32],
    #[serde(skip)]
    spec: ObjectSpec,
}

#[derive(Default, Serialize)]
pub(crate) struct Page {
    pub records: Vec<Record>,
    pub findings: Vec<Finding>,
}

impl From<&Page> for Diagnostics {
    fn from(page: &Page) -> Self {
        let mut diagnostics = Self::default();
        for finding in &page.findings {
            if finding.blocking {
                diagnostics.errors.push(finding.diagnostic.clone());
            } else {
                diagnostics.warnings.push(finding.diagnostic.clone());
            }
        }
        diagnostics
    }
}

#[derive(Debug, Default, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the import report serializes these flags as JSON booleans"
)]
pub struct Summary {
    pub complete: bool,
    pub eligible: bool,
    pub bodies_verified: bool,
    pub original_names_verified: bool,
    pub buckets: u64,
    pub objects: u64,
    pub eligible_buckets: u64,
    pub eligible_objects: u64,
    pub entries: u64,
    pub directories: u64,
    pub skipped_os_metadata: u64,
    pub blocking_findings: u64,
    pub stat_identity_drift_findings: u64,
    pub io_findings: u64,
    pub warnings: u64,
    pub bytes_hashed: u64,
    pub mixed_format_absence_verified: bool,
    pub fingerprint_format: u8,
    pub manifest_sha256: Option<String>,
    pub stat_observations_sha256: Option<String>,
    pub body_digests_sha256: Option<String>,
    /// The most severe blocking finding so far. The counters above are the report's view of the
    /// same findings; this one decides how a refused import fails.
    #[serde(skip)]
    pub(crate) worst_blocker: Option<Blocker>,
}

/// The kind of a blocking finding. A refused import reports its most severe kind: stat drift that
/// a retry resolves ranks below a source I/O failure, which ranks below anything that needs the
/// source itself repaired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Blocker {
    StatDrift,
    Io,
    Source,
}

impl Summary {
    fn block(&mut self, blocker: Blocker) {
        self.blocking_findings += 1;
        self.worst_blocker = self.worst_blocker.max(Some(blocker));
    }
}

pub(super) struct Reporter {
    send: PageSender,
    page: Page,
    pub summary: Summary,
    manifest: Context,
    observations: Context,
    body_manifest: Context,
    baseline: Option<filesystem::Baseline>,
    purpose: ScanPurpose,
}

enum PageSender {
    Sync(std::sync::mpsc::SyncSender<Page>),
    Async(mpsc::Sender<Page>),
}

impl PageSender {
    fn send(&self, page: Page) -> Result<(), ImportFailure> {
        match self {
            Self::Sync(send) => send.send(page).map_err(|_| ImportFailure::ReportClosed),
            Self::Async(send) => send
                .blocking_send(page)
                .map_err(|_| ImportFailure::ReportClosed),
        }
    }
}

impl Reporter {
    fn flush(&mut self) -> Result<(), ImportFailure> {
        if !self.page.records.is_empty() || !self.page.findings.is_empty() {
            self.send.send(std::mem::take(&mut self.page))?;
        }
        Ok(())
    }
    fn boundary(&mut self) -> Result<(), ImportFailure> {
        let limit = if self.purpose == ScanPurpose::Publish {
            PUBLICATION_PAGE_SIZE
        } else {
            PAGE_SIZE
        };
        if self.page.records.len() + self.page.findings.len() >= limit {
            self.flush()?;
        }
        Ok(())
    }
    pub(crate) fn record(&mut self, record: Record) -> Result<(), ImportFailure> {
        if let Some(baseline) = &mut self.baseline {
            baseline.check(&record)?;
        }
        self.page.records.push(record);
        self.boundary()
    }
    pub(crate) fn finding(
        &mut self,
        path: &str,
        field: &str,
        reason: impl Into<String>,
        fix: impl Into<String>,
        severity: Severity,
        original: Option<String>,
    ) -> Result<(), ImportFailure> {
        let blocking = severity == Severity::Blocking;
        if blocking {
            self.summary.block(Blocker::Source);
        } else {
            self.summary.warnings += 1;
        }
        self.page.findings.push(Finding {
            diagnostic: Diagnostic::new(DiagnosticSource::Import, format!("{path}:{field}"), fix),
            reason: reason.into(),
            blocking,
            original,
            changed_fields: Vec::new(),
        });
        self.boundary()
    }
    /// Reports a blocking finding for one entry and returns its marker.
    pub(crate) fn block(
        &mut self,
        path: &str,
        field: &str,
        reason: impl Into<String>,
        fix: impl Into<String>,
        original: Option<String>,
    ) -> Result<Reported, ImportFailure> {
        self.finding(path, field, reason, fix, Severity::Blocking, original)?;
        Ok(Reported)
    }
    pub(crate) fn failure(
        &mut self,
        path: &str,
        failure: &ImportFailure,
    ) -> Result<(), ImportFailure> {
        if matches!(failure, ImportFailure::Io(_)) {
            self.summary.io_findings += 1;
        }
        if failure.retryable_stat_drift() {
            self.summary.stat_identity_drift_findings += 1;
        }
        let fields = if let ImportFailure::SourceChanged { fields } = failure {
            fields.clone()
        } else {
            Vec::new()
        };
        self.page.findings.push(Finding {
            diagnostic: Diagnostic::new(
                DiagnosticSource::Import,
                format!("{path}:{}", failure.field()),
                failure.fix(),
            ),
            reason: failure.to_string(),
            blocking: true,
            original: None,
            changed_fields: fields,
        });
        let blocker = match failure {
            ImportFailure::Io(_) => Blocker::Io,
            ImportFailure::SourceChanged { .. } if failure.retryable_stat_drift() => {
                Blocker::StatDrift
            }
            ImportFailure::SourceChanged { .. }
            | ImportFailure::Json(_)
            | ImportFailure::Corrupt { .. }
            | ImportFailure::ReportClosed => Blocker::Source,
        };
        self.summary.block(blocker);
        self.boundary()
    }
    pub(crate) fn manifest(&mut self, kind: &str, path: &str, fields: &[&[u8]]) {
        digest_fields(&mut self.manifest, kind, path, fields);
    }

    pub(crate) fn verified_body(&mut self, path: &str, hashes: Hashes) {
        digest_fields(
            &mut self.body_manifest,
            "body",
            path,
            &[&hashes.md5, &hashes.crc32c.to_be_bytes()],
        );
    }
    pub(crate) fn observations(&mut self, path: &str, fingerprint: &Fingerprint) {
        digest_fields(
            &mut self.observations,
            "stat",
            path,
            &[
                &[u8::from(fingerprint.inode.is_some())],
                &fingerprint.inode.unwrap_or_default().to_be_bytes(),
                &[u8::from(fingerprint.device.is_some())],
                &fingerprint.device.unwrap_or_default().to_be_bytes(),
                &[u8::from(fingerprint.ctime_ns.is_some())],
                &fingerprint.ctime_ns.unwrap_or_default().to_be_bytes(),
            ],
        );
    }
}

fn digest_fields(context: &mut Context, kind: &str, path: &str, fields: &[&[u8]]) {
    for bytes in [kind.as_bytes(), path.as_bytes()] {
        context.update(&(bytes.len() as u64).to_be_bytes());
        context.update(bytes);
    }
    context.update(&(fields.len() as u64).to_be_bytes());
    for bytes in fields {
        context.update(&(bytes.len() as u64).to_be_bytes());
        context.update(bytes);
    }
}

fn digest_context(domain: &[u8]) -> Context {
    let mut context = Context::new(&SHA256);
    context.update(b"gcs-mimic-source-fingerprint\0");
    context.update(&[FINGERPRINT_FORMAT]);
    context.update(domain);
    context
}

fn finish_digest(context: Context) -> String {
    crate::hashing::encode_lower(context.finish().as_ref())
}

pub fn inspect(
    options: Options,
    mut output: impl Write,
    mut diagnostics: impl Write,
) -> Result<Summary, ImportError> {
    inspect_with_xattr(
        options,
        &mut output,
        &mut diagnostics,
        crate::fsutil::read_source_metadata,
    )
}

fn inspect_with_xattr(
    options: Options,
    mut output: impl Write,
    mut diagnostics: impl Write,
    xattr: reader::XattrReader,
) -> Result<Summary, ImportError> {
    options.validate()?;
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({"event":crate::diagnostics::Event::ImportReport, "format_version":1,
        "source_format": match options.source_format {
            SourceFormat::Filesystem => "fake-gcs-server-filesystem",
            SourceFormat::Snapshot => "fake-gcs-server-memory-snapshot",
        }, "source_revision":SOURCE_REVISION,
        "metadata_mode":(options.source_format == SourceFormat::Filesystem).then_some(options.metadata_mode),
        "source_platform":(options.source_format == SourceFormat::Filesystem).then_some(options.source_platform),
        "names_recovered_from_normalized_disk_paths":options.source_format == SourceFormat::Filesystem,
        "original_names_verified":false,
        "preservation_scope": match options.source_format {
            SourceFormat::Filesystem => "recoverable current live disk state; no history, IAM or original-name losslessness",
            SourceFormat::Snapshot => "complete quiesced memory capture with explicit live/archive membership; generated native ETags/metagenerations/project",
        },
        "dry_run":true, "verify_bodies":options.verify_bodies, "max_object_bytes":options.max_object_bytes,
        "max_entries":options.max_entries,"max_depth":options.max_depth,"page_size":PAGE_SIZE,"channel_pages":2}),
    )?;
    writeln!(output)?;
    let (send, receive) = std::sync::mpsc::sync_channel(2);
    let worker = spawn_scan(
        options,
        xattr,
        PageSender::Sync(send),
        None,
        ScanPurpose::Inspect,
    )?;
    let write_result = write_pages(&receive, &mut output, &mut diagnostics);
    drop(receive);
    let joined = worker.blocking_recv().map_err(io::Error::other)?;
    write_result?;
    let summary = joined?;
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({"event":crate::diagnostics::Event::ImportSummary, "summary":summary}),
    )?;
    writeln!(output)?;
    Ok(summary)
}

/// Writes each page and its diagnostics as the scanner sends them.
fn write_pages(
    pages: &std::sync::mpsc::Receiver<Page>,
    output: &mut impl Write,
    diagnostics: &mut impl Write,
) -> io::Result<()> {
    for page in pages {
        let batch = Diagnostics::from(&page);
        serde_json::to_writer(
            &mut *output,
            &serde_json::json!({"event":crate::diagnostics::Event::ImportPage, "page": page}),
        )?;
        writeln!(output)?;
        batch.write(&mut *diagnostics)?;
    }
    Ok(())
}

type ScanWorker = tokio::sync::oneshot::Receiver<Result<Summary, ImportFailure>>;
type Scan = (mpsc::Receiver<Page>, ScanWorker);

fn scan_with_baseline(
    options: Options,
    xattr: reader::XattrReader,
    baseline: Option<(fs::File, filesystem::BaselineMode)>,
    purpose: ScanPurpose,
) -> io::Result<Scan> {
    let (send, receive) = mpsc::channel(2);
    let worker = spawn_scan(options, xattr, PageSender::Async(send), baseline, purpose)?;
    Ok((receive, worker))
}

fn spawn_scan(
    options: Options,
    xattr: reader::XattrReader,
    send: PageSender,
    baseline: Option<(fs::File, filesystem::BaselineMode)>,
    purpose: ScanPurpose,
) -> io::Result<ScanWorker> {
    let (completion, worker) = tokio::sync::oneshot::channel();
    crate::task::spawn_thread("import-inspection", move || {
        let result = crate::task::catch_job("import-inspection", || {
            let mut reporter = Reporter {
                send,
                page: Page::default(),
                summary: Summary {
                    mixed_format_absence_verified: true,
                    ..Summary::default()
                },
                manifest: digest_context(b"export"),
                observations: digest_context(b"stat"),
                body_manifest: digest_context(b"bodies"),
                baseline: baseline
                    .map(|(file, mode)| filesystem::Baseline::open(file, mode))
                    .transpose()?,
                purpose,
            };
            reporter.manifest(
                "options",
                "",
                &option_fields(&options).each_ref().map(Vec::as_slice),
            );
            let result = match options.source_format {
                SourceFormat::Filesystem => reader::scan(&options, &mut reporter, xattr),
                SourceFormat::Snapshot => snapshot::scan(&options, &mut reporter),
            };
            match result {
                Ok(()) => reporter.summary.complete = true,
                Err(ImportFailure::ReportClosed) => return Err(ImportFailure::ReportClosed),
                Err(error) => reporter.failure(".", &error)?,
            }
            reporter.summary.eligible =
                reporter.summary.complete && reporter.summary.blocking_findings == 0;
            reporter.summary.bodies_verified = options.verify_bodies && reporter.summary.eligible;
            reporter.manifest(
                "totals",
                "",
                &[
                    &reporter.summary.entries.to_be_bytes(),
                    &reporter.summary.skipped_os_metadata.to_be_bytes(),
                ],
            );
            reporter.summary.fingerprint_format = FINGERPRINT_FORMAT;
            if reporter.summary.eligible {
                reporter.summary.manifest_sha256 = Some(finish_digest(reporter.manifest.clone()));
                reporter.summary.stat_observations_sha256 =
                    Some(finish_digest(reporter.observations.clone()));
            }
            if reporter.summary.bodies_verified {
                reporter.summary.body_digests_sha256 =
                    Some(finish_digest(reporter.body_manifest.clone()));
            }
            reporter.flush()?;
            if let Some(baseline) = &mut reporter.baseline {
                baseline.finish()?;
            }
            Ok(reporter.summary)
        })
        .unwrap_or_else(|cause| {
            Err(
                io::Error::other(crate::storage::failure::BackendCause::with_source(
                    "import scanner panicked",
                    cause,
                ))
                .into(),
            )
        });
        let _ = completion.send(result);
    })?;
    Ok(worker)
}

fn option_fields(options: &Options) -> [Vec<u8>; 6] {
    [
        SOURCE_REVISION.as_bytes().to_vec(),
        vec![match options.metadata_mode {
            MetadataMode::Xattr => 0,
            MetadataMode::Sidecar => 1,
        }],
        vec![match options.source_platform {
            SourcePlatform::Unix => 0,
            SourcePlatform::Windows => 1,
        }],
        options.max_object_bytes.to_be_bytes().to_vec(),
        identity_limit(options.max_entries).to_be_bytes().to_vec(),
        identity_limit(options.max_depth).to_be_bytes().to_vec(),
    ]
}

/// Zero cannot be a limit, so it identifies an import that has none.
fn identity_limit(limit: Option<NonZeroUsize>) -> u64 {
    limit.map_or(0, |limit| limit.get() as u64)
}

/// A value option of `import fake-gcs-server`; the flag and its value are separate arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ValueFlag {
    Source,
    DataDir,
    MetadataMode,
    SourcePlatform,
    SourceFormat,
    MaxObjectBytes,
    MaxEntries,
    MaxDepth,
}

impl ValueFlag {
    const ALL: [Self; 8] = [
        Self::Source,
        Self::DataDir,
        Self::MetadataMode,
        Self::SourcePlatform,
        Self::SourceFormat,
        Self::MaxObjectBytes,
        Self::MaxEntries,
        Self::MaxDepth,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "--source",
            Self::DataDir => "--data-dir",
            Self::MetadataMode => "--metadata-mode",
            Self::SourcePlatform => "--source-platform",
            Self::SourceFormat => "--source-format",
            Self::MaxObjectBytes => "--max-object-bytes",
            Self::MaxEntries => "--max-entries",
            Self::MaxDepth => "--max-depth",
        }
    }

    fn from_flag(flag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|known| known.as_str() == flag)
    }
}

/// A refusal of the command line, named by the argument it blames.
fn refusal(key: &str, fix: impl Into<String>) -> Diagnostics {
    Diagnostics::error(DiagnosticSource::Cli, key, fix)
}

fn parse<T>(flag: ValueFlag, value: &str) -> Result<T, Diagnostics>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    value
        .parse()
        .map_err(|fix: T::Err| refusal(flag.as_str(), fix.to_string()))
}

/// The command line once its flags were read. Values stay text until the option that owns them
/// parses them, so each failure names its own option.
struct Arguments<'a> {
    values: BTreeMap<ValueFlag, &'a str>,
    dry_run: bool,
    verify_bodies: bool,
}

impl<'a> Arguments<'a> {
    fn read(args: &'a [String]) -> Result<Self, Diagnostics> {
        let mut arguments = Self {
            values: BTreeMap::new(),
            dry_run: false,
            verify_bodies: false,
        };
        let mut args = args.iter().map(String::as_str);
        while let Some(key) = args.next() {
            match (key, ValueFlag::from_flag(key)) {
                ("--dry-run", _) if !arguments.dry_run => arguments.dry_run = true,
                ("--verify-bodies", _) if !arguments.verify_bodies => {
                    arguments.verify_bodies = true;
                }
                (_, Some(flag)) => {
                    let value = args
                        .next()
                        .filter(|value| !value.is_empty() && !value.starts_with("--"))
                        .ok_or_else(|| {
                            refusal(
                                key,
                                "Supply exactly one nonempty value; see import fake-gcs-server --help.",
                            )
                        })?;
                    if arguments.values.insert(flag, value).is_some() {
                        return Err(refusal(key, "Remove the duplicate option."));
                    }
                }
                (_, None) => {
                    return Err(refusal(
                        key,
                        "Remove the unknown or duplicate option; see import fake-gcs-server --help.",
                    ));
                }
            }
        }
        Ok(arguments)
    }

    fn value(&self, flag: ValueFlag) -> Option<&'a str> {
        self.values.get(&flag).copied()
    }

    fn contains(&self, flag: ValueFlag) -> bool {
        self.values.contains_key(&flag)
    }

    fn required(&self, flag: ValueFlag) -> Result<&'a str, Diagnostics> {
        self.value(flag).ok_or_else(|| {
            refusal(
                flag.as_str(),
                "Supply the required option; see import fake-gcs-server --help.",
            )
        })
    }

    /// Parses an option's value, if it has one; the error text of its `string_enum!` type is the
    /// fix.
    fn parsed<T>(&self, flag: ValueFlag) -> Result<Option<T>, Diagnostics>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        self.value(flag).map(|value| parse(flag, value)).transpose()
    }

    fn parsed_required<T>(&self, flag: ValueFlag) -> Result<T, Diagnostics>
    where
        T: FromStr,
        T::Err: fmt::Display,
    {
        parse(flag, self.required(flag)?)
    }

    /// The limit an option names, at most `maximum`.
    fn limit(&self, flag: ValueFlag, maximum: u64) -> Result<Option<u64>, Diagnostics> {
        self.value(flag)
            .map(|value| {
                value
                    .parse::<u64>()
                    .ok()
                    .filter(|limit| *limit <= maximum)
                    .ok_or_else(|| {
                        refusal(
                            flag.as_str(),
                            format!(
                                "Use an integer in 0..={maximum}; see import fake-gcs-server --help."
                            ),
                        )
                    })
            })
            .transpose()
    }
}

/// Applies the limit options; 0 means no limit, which for a body is Cloud Storage's own.
fn apply_limits(options: &mut Options, arguments: &Arguments<'_>) -> Result<(), ImportError> {
    if let Some(bytes) = arguments.limit(ValueFlag::MaxObjectBytes, MAX_OBJECT_BYTES)? {
        options.max_object_bytes = if bytes == 0 { MAX_OBJECT_BYTES } else { bytes };
    }
    if let Some(entries) = arguments.limit(ValueFlag::MaxEntries, u64::MAX)? {
        options.max_entries = NonZeroUsize::new(usize::try_from(entries)?);
    }
    if let Some(depth) = arguments.limit(ValueFlag::MaxDepth, MAX_TREE_DEPTH as u64)? {
        options.max_depth = NonZeroUsize::new(usize::try_from(depth)?);
    }
    Ok(())
}

/// The metadata mode and source platform a filesystem source names; a snapshot names neither.
fn source_layout(
    arguments: &Arguments<'_>,
    source_format: SourceFormat,
) -> Result<(MetadataMode, SourcePlatform), Diagnostics> {
    match source_format {
        SourceFormat::Snapshot => {
            if arguments.contains(ValueFlag::MetadataMode)
                || arguments.contains(ValueFlag::SourcePlatform)
            {
                return Err(refusal(
                    ValueFlag::SourceFormat.as_str(),
                    "Snapshot names and attributes are explicit; remove --metadata-mode and --source-platform.",
                ));
            }
            if arguments.contains(ValueFlag::MaxDepth) {
                return Err(refusal(
                    ValueFlag::MaxDepth.as_str(),
                    "A snapshot holds rows, not directories; remove --max-depth.",
                ));
            }
            Ok((MetadataMode::Sidecar, SourcePlatform::Unix))
        }
        SourceFormat::Filesystem => Ok((
            arguments.parsed_required(ValueFlag::MetadataMode)?,
            arguments.parsed_required(ValueFlag::SourcePlatform)?,
        )),
    }
}

pub fn cli(args: &[String]) -> Result<(), ImportError> {
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    let arguments = Arguments::read(args)?;
    if arguments.verify_bodies && !arguments.dry_run {
        return Err(refusal(
            "--verify-bodies",
            "Use --dry-run --verify-bodies for inspection; actual import always verifies bodies.",
        )
        .into());
    }
    let source_format: SourceFormat = arguments
        .parsed(ValueFlag::SourceFormat)?
        .unwrap_or_default();
    let (metadata_mode, source_platform) = source_layout(&arguments, source_format)?;
    let mut options = Options::new(
        arguments.required(ValueFlag::Source)?.into(),
        arguments.required(ValueFlag::DataDir)?.into(),
        metadata_mode,
        source_platform,
    );
    options.source_format = source_format;
    options.verify_bodies = arguments.verify_bodies;
    apply_limits(&mut options, &arguments)?;
    let summary = if arguments.dry_run {
        inspect(options, io::stdout().lock(), io::stderr().lock())?
    } else {
        execute(options, io::stdout().lock(), io::stderr().lock()).map_err(|error| {
            Diagnostics::error(
                DiagnosticSource::Import,
                "import",
                format!("Import failed: {error}. Keep the source snapshot unchanged; resolve the reported cause and retry only the same source/options. Terminal failures require a new empty destination; never remove only the import marker."),
            )
        })?
    };
    if !summary.eligible {
        return Err(refusal("import", "Inspection found blocking findings; apply the reported remedies and inspect a stable snapshot again.").into());
    }
    Ok(())
}
