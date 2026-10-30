#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Read-only inspection of pinned fake-gcs-server durable filesystem snapshots.

pub(crate) mod codec;
mod filesystem;
mod reader;
#[cfg(test)]
mod tests;

use crate::{
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    hashing::Hashes,
    source_tree,
};
pub use filesystem::execute;
pub use reader::{BodyStream, ChangedField, Fingerprint, ImportFailure};
use ring::digest::{Context, SHA256};
use serde::Serialize;
use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};
use tokio::sync::mpsc;

pub(crate) const SOURCE_REVISION: &str = "896ece51e49eed0946089f3089f6343855c0d106";
pub(crate) const PAGE_SIZE: usize = 256;
const PUBLICATION_PAGE_SIZE: usize = 8;
pub(crate) const FINGERPRINT_FORMAT: u8 = 1;
pub(crate) const HELP: &str = "Usage: gcs-mimic import fake-gcs-server --source DIR --data-dir NEW
       --metadata-mode xattr|sidecar --source-platform unix|windows [--dry-run [--verify-bodies]]

Import a stopped writer's durable filesystem snapshot without changing the source.
NEW must be absent or empty, or contain the same unfinished import for automatic resume.
--dry-run inspects metadata without creating NEW; --verify-bodies also verifies its bodies.
Actual import always verifies body checksums and keeps unfinished destinations unservable.
Output is JSON Lines; diagnostics use stderr. Source and destination must be separate.
Only normalized disk names are recoverable; original API names and history cannot be verified.
Native bucket defaults/project and ETags are generated. Memory exports are unavailable.
No server configuration is read.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataMode {
    Xattr,
    Sidecar,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePlatform {
    Unix,
    Windows,
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

#[derive(Clone, Debug)]
pub struct Options {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub metadata_mode: MetadataMode,
    pub source_platform: SourcePlatform,
    pub verify_bodies: bool,
    pub max_object_bytes: u64,
    pub max_entries: usize,
    pub max_depth: usize,
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
            verify_bodies: false,
            max_object_bytes: 1_073_741_824,
            max_entries: 10_000,
            max_depth: 32,
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
        if self.metadata_mode == MetadataMode::Xattr
            && self.source_platform == SourcePlatform::Windows
        {
            issue(
                "--metadata-mode",
                "Windows source snapshots use sidecars; specify --metadata-mode sidecar.",
            );
        }
        if self.max_entries == 0 || self.max_depth > 32 {
            issue(
                "limits",
                "Use a positive entry limit and at most 32 directory levels.",
            );
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

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Record {
    Bucket {
        path: String,
        name: String,
        attributes: codec::BucketAttrs,
        generated: &'static [&'static str],
        observed_stat: String,
        #[serde(skip)]
        attributes_digest: [u8; 32],
    },
    Object {
        path: String,
        bucket: String,
        name: String,
        attributes: Box<codec::ObjectAttrs>,
        created_micros: i64,
        updated_micros: i64,
        fingerprint: Fingerprint,
        verified_hashes: Option<Hashes>,
        generated: &'static [&'static str],
        #[serde(skip)]
        source_file: Option<fs::File>,
        #[serde(skip)]
        attributes_digest: [u8; 32],
    },
    Directory {
        path: String,
    },
}

#[derive(Default, Serialize)]
pub(crate) struct Page {
    pub records: Vec<Record>,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub complete: bool,
    pub eligible: bool,
    pub bodies_verified: bool,
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
            self.summary.blocking_findings += 1;
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
        let (field, fix) = match failure {
            ImportFailure::Corrupt { field, fix, .. } => (*field, fix.as_ref()),
            ImportFailure::SourceChanged { .. } => (
                "fingerprint",
                "Stop the source writer and inspect an immutable snapshot again; retain the changed-field evidence.",
            ),
            ImportFailure::Io(_) => (
                "io",
                "Check read/search permissions and filesystem availability for this relative entry; retry after resolving the reported I/O error.",
            ),
            ImportFailure::ReportClosed => (
                "report",
                "Keep the JSON report consumer open through the final summary.",
            ),
        };
        self.page.findings.push(Finding {
            diagnostic: Diagnostic::new(DiagnosticSource::Import, format!("{path}:{field}"), fix),
            reason: failure.to_string(),
            blocking: true,
            original: None,
            changed_fields: fields,
        });
        self.summary.blocking_findings += 1;
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
) -> Result<Summary, Box<dyn std::error::Error>> {
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
) -> Result<Summary, Box<dyn std::error::Error>> {
    options.validate()?;
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({"event":"import_report", "format_version":1,
        "source_format":"fake-gcs-server-filesystem", "source_revision":SOURCE_REVISION,
        "metadata_mode":options.metadata_mode, "source_platform":options.source_platform,
        "names_recovered_from_normalized_disk_paths":true, "original_names_verified":false,
        "preservation_scope":"recoverable current live disk state only; no history, IAM or original-name losslessness",
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
    let write_result = (|| -> io::Result<()> {
        for page in &receive {
            let mut batch = Diagnostics::default();
            for finding in &page.findings {
                if finding.blocking {
                    batch.errors.push(finding.diagnostic.clone());
                } else {
                    batch.warnings.push(finding.diagnostic.clone());
                }
            }
            serde_json::to_writer(
                &mut output,
                &serde_json::json!({"event":"import_page", "page": page}),
            )?;
            writeln!(output)?;
            batch.write(&mut diagnostics)?;
        }
        Ok(())
    })();
    drop(receive);
    let joined = worker
        .join()
        .map_err(|_| io::Error::other("import worker panicked"))?;
    write_result?;
    let summary = joined?;
    serde_json::to_writer(
        &mut output,
        &serde_json::json!({"event":"import_summary", "summary":summary}),
    )?;
    writeln!(output)?;
    Ok(summary)
}

type ScanWorker = std::thread::JoinHandle<Result<Summary, ImportFailure>>;
type Scan = (mpsc::Receiver<Page>, ScanWorker);

fn scan_with_baseline(
    options: Options,
    xattr: reader::XattrReader,
    baseline: Option<(PathBuf, filesystem::BaselineMode)>,
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
    baseline: Option<(PathBuf, filesystem::BaselineMode)>,
    purpose: ScanPurpose,
) -> io::Result<ScanWorker> {
    let worker = std::thread::Builder::new()
        .name("import-inspection".into())
        .spawn(move || {
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
                    .map(|(path, mode)| filesystem::Baseline::open(&path, mode))
                    .transpose()?,
                purpose,
            };
            reporter.manifest(
                "options",
                "",
                &[
                    SOURCE_REVISION.as_bytes(),
                    &[match options.metadata_mode {
                        MetadataMode::Xattr => 0,
                        MetadataMode::Sidecar => 1,
                    }],
                    &[match options.source_platform {
                        SourcePlatform::Unix => 0,
                        SourcePlatform::Windows => 1,
                    }],
                    &options.max_object_bytes.to_be_bytes(),
                    &(options.max_entries as u64).to_be_bytes(),
                    &(options.max_depth as u64).to_be_bytes(),
                ],
            );
            match reader::scan(&options, &mut reporter, xattr) {
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
            Ok(reporter.summary)
        })?;
    Ok(worker)
}

pub fn cli(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args == ["--help"] {
        println!("{HELP}");
        return Ok(());
    }
    let invalid = |key: &str, fix: &str| Diagnostics::error(DiagnosticSource::Cli, key, fix);
    let mut values = std::collections::BTreeMap::new();
    let (mut dry_run, mut verify) = (false, false);
    let mut index = 0;
    while let Some(key) = args.get(index) {
        match key.as_str() {
            "--dry-run" if !dry_run => dry_run = true,
            "--verify-bodies" if !verify => verify = true,
            "--source" | "--data-dir" | "--metadata-mode" | "--source-platform" => {
                index += 1;
                let value = args
                    .get(index)
                    .filter(|v| !v.is_empty() && !v.starts_with("--"))
                    .ok_or_else(|| {
                        invalid(
                            key,
                            "Supply exactly one nonempty value; see import fake-gcs-server --help.",
                        )
                    })?;
                if values.insert(key.as_str(), value.as_str()).is_some() {
                    return Err(invalid(key, "Remove the duplicate option.").into());
                }
            }
            _ => {
                return Err(invalid(
                    key,
                    "Remove the unknown or duplicate option; see import fake-gcs-server --help.",
                )
                .into());
            }
        }
        index += 1;
    }
    if verify && !dry_run {
        return Err(invalid(
            "--verify-bodies",
            "Use --dry-run --verify-bodies for inspection; actual import always verifies bodies.",
        )
        .into());
    }
    let required = |key| {
        values.get(key).copied().ok_or_else(|| {
            invalid(
                key,
                "Supply the required option; see import fake-gcs-server --help.",
            )
        })
    };
    let mode = match required("--metadata-mode")? {
        "xattr" => MetadataMode::Xattr,
        "sidecar" => MetadataMode::Sidecar,
        _ => {
            return Err(invalid(
                "--metadata-mode",
                "Use xattr or sidecar, matching the entire source root.",
            )
            .into());
        }
    };
    let platform = match required("--source-platform")? {
        "unix" => SourcePlatform::Unix,
        "windows" => SourcePlatform::Windows,
        _ => {
            return Err(invalid(
                "--source-platform",
                "Use unix or windows, matching the source path platform.",
            )
            .into());
        }
    };
    let mut options = Options::new(
        required("--source")?.into(),
        required("--data-dir")?.into(),
        mode,
        platform,
    );
    options.verify_bodies = verify;
    let summary = if dry_run {
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
        return Err(invalid("import", "Inspection found blocking findings; apply the reported remedies and inspect a stable snapshot again.").into());
    }
    Ok(())
}
