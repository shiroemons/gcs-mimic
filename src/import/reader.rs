#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{
    Checked, MetadataMode, Options, Record, Reported, Reporter, ScanPurpose, Severity,
    SourcePlatform,
    codec::{self, BucketAttrs, ObjectAttrs},
};
use crate::{
    acl::AclEntry,
    bucket::StorageClass,
    fsutil::{SOURCE_METADATA_CAP, SourceMetadataBuffer, SourceMetadataError},
    hashing::{Hasher, Hashes},
    object::generation::Generation,
    source_tree::{self, WalkError, Walker},
    storage::failure::BackendCause,
};
use serde::Serialize;
use std::{
    borrow::Cow,
    collections::BTreeMap,
    fmt,
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

pub(super) type XattrReader = for<'a> fn(
    &File,
    &'a mut SourceMetadataBuffer,
) -> Result<Option<&'a [u8]>, SourceMetadataError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangedField {
    Size,
    Mtime,
    Inode,
    Ctime,
    Metadata,
}

/// The report key of a source failure: diagnostics and the import report name it
/// `{path}:{field}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorruptField {
    Name,
    Metadata,
    UserMetadata,
    Entry,
    Limits,
    Size,
    BucketAttrs,
    ObjectAttrs,
    Snapshot,
}

impl CorruptField {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Metadata => "metadata",
            Self::UserMetadata => "user.metadata",
            Self::Entry => "entry",
            Self::Limits => "limits",
            Self::Size => "Size",
            Self::BucketAttrs => "BucketAttrs",
            Self::ObjectAttrs => "ObjectAttrs",
            Self::Snapshot => "snapshot",
        }
    }
}

#[derive(Debug)]
pub enum ImportFailure {
    SourceChanged {
        fields: Vec<ChangedField>,
    },
    Io(io::Error),
    Json(serde_json::Error),
    Corrupt {
        field: CorruptField,
        reason: BackendCause,
        fix: Cow<'static, str>,
    },
    ReportClosed,
}
impl ImportFailure {
    pub(super) fn corrupt(
        field: CorruptField,
        reason: impl Into<BackendCause>,
        fix: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self::Corrupt {
            field,
            reason: reason.into(),
            fix: fix.into(),
        }
    }
    /// The report key that names this failure after its path.
    pub(super) fn field(&self) -> &'static str {
        match self {
            Self::Corrupt { field, .. } => field.as_str(),
            Self::SourceChanged { .. } => "fingerprint",
            Self::Io(_) => "io",
            Self::Json(_) => "snapshot_json",
            Self::ReportClosed => "report",
        }
    }
    /// The remedy the report gives for this failure.
    pub(super) fn fix(&self) -> &str {
        match self {
            Self::Corrupt { fix, .. } => fix,
            Self::SourceChanged { .. } => {
                "Stop the source writer and inspect an immutable snapshot again; retain the changed-field evidence."
            }
            Self::Io(_) => {
                "Check read/search permissions and filesystem availability for this relative entry; retry after resolving the reported I/O error."
            }
            Self::Json(_) => {
                "Restore the complete source snapshot with known, unique fields and the declared JSON types."
            }
            Self::ReportClosed => "Keep the JSON report consumer open through the final summary.",
        }
    }
    pub fn retryable_stat_drift(&self) -> bool {
        matches!(self, Self::SourceChanged { fields } if !fields.is_empty() && fields.iter().all(|f| matches!(f, ChangedField::Inode | ChangedField::Ctime)))
    }
}
impl fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceChanged { fields } => write!(f, "source_changed: {fields:?}"),
            Self::Io(error) => write!(f, "source I/O failure ({:?})", error.kind()),
            Self::Json(error) => write!(f, "invalid source snapshot JSON: {error}"),
            Self::Corrupt { reason, .. } => reason.fmt(f),
            Self::ReportClosed => f.write_str("report output closed"),
        }
    }
}
impl std::error::Error for ImportFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Corrupt { reason, .. } => reason.source(),
            _ => None,
        }
    }
}
impl From<serde_json::Error> for ImportFailure {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<io::Error> for ImportFailure {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Fingerprint {
    pub size: u64,
    pub mtime_ns: i128,
    pub ctime_ns: Option<i128>,
    pub inode: Option<u64>,
    pub device: Option<u64>,
}
impl Fingerprint {
    pub fn of(metadata: &Metadata) -> io::Result<Self> {
        let mtime_ns = match metadata.modified()?.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos() as i128,
            Err(e) => -(e.duration().as_nanos() as i128),
        };
        #[cfg(unix)]
        let (ctime_ns, inode, device) = {
            use std::os::unix::fs::MetadataExt;
            (
                Some(
                    i128::from(metadata.ctime()) * 1_000_000_000
                        + i128::from(metadata.ctime_nsec()),
                ),
                Some(metadata.ino()),
                Some(metadata.dev()),
            )
        };
        #[cfg(not(unix))]
        let (ctime_ns, inode, device) = (None, None, None);
        Ok(Self {
            size: metadata.len(),
            mtime_ns,
            ctime_ns,
            inode,
            device,
        })
    }
    /// Fingerprint of an open file. std metadata has no file identity or change
    /// time on Windows, so take the volume, file index and change time from the
    /// handle there; otherwise a same-size, same-mtime replacement goes unnoticed.
    pub(crate) fn of_file(file: &File) -> io::Result<Self> {
        let fingerprint = Self::of(&file.metadata()?)?;
        #[cfg(windows)]
        let fingerprint = {
            let identity = crate::fsutil::stat_identity(file).map_err(io::Error::other)?;
            Self {
                ctime_ns: Some(identity.changed_ns),
                inode: Some(identity.inode),
                device: Some(identity.device),
                ..fingerprint
            }
        };
        Ok(fingerprint)
    }
    /// Fingerprint of the entry a path names now, without following a symlink.
    pub(crate) fn of_path(path: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            };
            let file = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path)?;
            Self::of_file(&file)
        }
        #[cfg(not(windows))]
        Self::of(&fs::symlink_metadata(path)?)
    }
    pub(crate) fn check(&self, other: &Self) -> Result<(), ImportFailure> {
        let mut fields = Vec::new();
        if self.size != other.size {
            fields.push(ChangedField::Size);
        }
        if self.mtime_ns != other.mtime_ns {
            fields.push(ChangedField::Mtime);
        }
        if self.ctime_ns != other.ctime_ns {
            fields.push(ChangedField::Ctime);
        }
        if self.inode != other.inode || self.device != other.device {
            fields.push(ChangedField::Inode);
        }
        if fields.is_empty() {
            Ok(())
        } else {
            Err(ImportFailure::SourceChanged { fields })
        }
    }
}

fn open_regular(path: &Path) -> Result<File, ImportFailure> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(ImportFailure::corrupt(
            CorruptField::Entry,
            "expected a regular file, not a symlink or special entry",
            "Replace the entry with the original regular snapshot file; do not follow symlinks.",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(ImportFailure::corrupt(
            CorruptField::Entry,
            "opened source is not a regular file",
            "Restore a regular snapshot file and stop concurrent filesystem changes.",
        ));
    }
    Ok(file)
}

pub struct BodyStream {
    file: File,
    path: PathBuf,
    before: Fingerprint,
    read: u64,
    max_bytes: u64,
}
impl BodyStream {
    pub fn open(path: &Path, max_bytes: u64) -> Result<Self, ImportFailure> {
        let file = open_regular(path)?;
        Self::from_file(file, path, max_bytes)
    }
    pub(super) fn from_file(
        file: File,
        path: &Path,
        max_bytes: u64,
    ) -> Result<Self, ImportFailure> {
        let before = Fingerprint::of_file(&file)?;
        if before.size > max_bytes {
            return Err(ImportFailure::corrupt(
                CorruptField::Size,
                "body exceeds max_object_bytes",
                "Raise --max-object-bytes, or use a source body within the inclusive object-size cap; do not truncate stored data.",
            ));
        }
        Ok(Self {
            file,
            path: path.into(),
            before,
            read: 0,
            max_bytes,
        })
    }
    pub(super) fn fingerprint(&self) -> &Fingerprint {
        &self.before
    }
    pub(super) fn into_file(self) -> File {
        self.file
    }
    pub fn next_chunk<'a>(
        &mut self,
        buffer: &'a mut [u8; 65_536],
    ) -> Result<&'a [u8], ImportFailure> {
        let limit = usize::try_from(
            self.max_bytes
                .saturating_sub(self.read)
                .saturating_add(1)
                .min(buffer.len() as u64),
        )
        .map_err(io::Error::other)?;
        let length = self.file.read(
            buffer
                .get_mut(..limit)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?,
        )?;
        self.read = self.read.checked_add(length as u64).ok_or_else(|| {
            ImportFailure::corrupt(
                CorruptField::Size,
                "body length overflow",
                "Use a supported bounded body size.",
            )
        })?;
        if self.read > self.max_bytes {
            return Err(ImportFailure::SourceChanged {
                fields: vec![ChangedField::Size],
            });
        }
        Ok(buffer
            .get(..length)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?)
    }
    pub fn check_unchanged(&self) -> Result<(), ImportFailure> {
        let mut changed = Vec::new();
        for after in [
            Fingerprint::of_file(&self.file)?,
            Fingerprint::of_path(&self.path)?,
        ] {
            if let Err(ImportFailure::SourceChanged { fields }) = self.before.check(&after) {
                changed.extend(fields);
            }
        }
        changed.sort();
        changed.dedup();
        if changed.is_empty() {
            Ok(())
        } else {
            Err(ImportFailure::SourceChanged { fields: changed })
        }
    }
}

fn read_json_file(path: &Path) -> Result<Vec<u8>, ImportFailure> {
    let mut file = open_regular(path)?;
    let before = Fingerprint::of_file(&file)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take((SOURCE_METADATA_CAP + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > SOURCE_METADATA_CAP {
        return Err(ImportFailure::corrupt(
            CorruptField::Metadata,
            "metadata exceeds 16 MiB",
            "Restore a metadata record of at most 16 MiB from the original export; do not truncate JSON.",
        ));
    }
    before.check(&Fingerprint::of_file(&file)?)?;
    before.check(&Fingerprint::of_path(path)?)?;
    Ok(bytes)
}

fn compare_json_file(path: &Path, expected: &[u8]) -> Result<(), ImportFailure> {
    let mut file = open_regular(path)?;
    let before = Fingerprint::of_file(&file)?;
    let mut buffer = [0; 4096];
    let mut offset = 0;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if expected.get(offset..offset + count) != buffer.get(..count) {
            return Err(ImportFailure::SourceChanged {
                fields: vec![ChangedField::Metadata],
            });
        }
        offset += count;
    }
    if offset != expected.len() {
        return Err(ImportFailure::SourceChanged {
            fields: vec![ChangedField::Metadata],
        });
    }
    before.check(&Fingerprint::of_file(&file)?)?;
    before.check(&Fingerprint::of_path(path)?)?;
    Ok(())
}

fn sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".metadata");
    name.into()
}

fn xattr_failure(error: SourceMetadataError) -> ImportFailure {
    match error {
        SourceMetadataError::Io(error) => ImportFailure::Io(error),
        SourceMetadataError::Unsupported => ImportFailure::corrupt(
            CorruptField::UserMetadata,
            "user.metadata reading is unsupported",
            "Inspect the original xattr snapshot on an xattr-capable filesystem; do not change metadata mode to hide missing attributes.",
        ),
        SourceMetadataError::TooLarge => ImportFailure::corrupt(
            CorruptField::UserMetadata,
            "user.metadata exceeds 16 MiB",
            "Restore a complete attribute of at most 16 MiB; do not truncate its JSON.",
        ),
    }
}

fn metadata_bytes(
    body: &BodyStream,
    options: &Options,
    buffer: &mut SourceMetadataBuffer,
    report: &mut Reporter,
    relative: &str,
    xattr: XattrReader,
) -> Result<Vec<u8>, ImportFailure> {
    match (options.metadata_mode, xattr(&body.file, buffer)) {
        (MetadataMode::Xattr, Ok(Some(bytes))) => Ok(bytes.to_vec()),
        (MetadataMode::Xattr, Ok(None)) => Err(ImportFailure::corrupt(
            CorruptField::UserMetadata,
            "missing user.metadata",
            "Restore the body's original user.metadata attribute from the xattr snapshot; a neighboring sidecar is not a fallback.",
        )),
        (MetadataMode::Xattr, Err(error)) => Err(xattr_failure(error)),
        (MetadataMode::Sidecar, result) => {
            match result {
                Ok(Some(bytes)) if !bytes.is_empty() => {
                    return Err(ImportFailure::corrupt(
                        CorruptField::UserMetadata,
                        "mixed metadata modes: user.metadata is nonempty in a sidecar snapshot",
                        "Supply a consistent sidecar snapshot with no persisted body xattrs, or select xattr only for an entirely xattr-based source.",
                    ));
                }
                Ok(_) => (),
                Err(SourceMetadataError::Unsupported) => {
                    report.summary.mixed_format_absence_verified = false;
                    report.finding(relative, "user.metadata", "xattr absence could not be verified on this filesystem", "Inspect the snapshot on a host supporting source xattrs before claiming complete mixed-format verification.", Severity::Warning, None)?;
                }
                Err(error) => return Err(xattr_failure(error)),
            }
            match read_json_file(&sidecar(&body.path)) {
                Err(ImportFailure::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                    Err(ImportFailure::corrupt(
                        CorruptField::Metadata,
                        "missing object metadata sidecar",
                        "Restore the neighboring <object>.metadata JSON from the same snapshot; body bytes alone do not contain attributes.",
                    ))
                }
                result => result,
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ObjectField {
    Generation,
    StorageClassInvalid,
    StorageClassUnsupported,
    ContentType,
    ContentEncoding,
    ContentDisposition,
    ContentLanguage,
    CacheControl,
    Metadata,
    Acl,
    Created,
    Updated,
    Deleted,
    CustomTime,
    RetentionMode,
    RetentionRetainUntil,
    Md5Hash,
    Crc32c,
}
impl ObjectField {
    fn name(self) -> &'static str {
        match self {
            Self::Generation => "Generation",
            Self::StorageClassInvalid | Self::StorageClassUnsupported => "StorageClass",
            Self::ContentType => "ContentType",
            Self::ContentEncoding => "ContentEncoding",
            Self::ContentDisposition => "ContentDisposition",
            Self::ContentLanguage => "ContentLanguage",
            Self::CacheControl => "CacheControl",
            Self::Metadata => "Metadata",
            Self::Acl => "ACL",
            Self::Created => "Created",
            Self::Updated => "Updated",
            Self::Deleted => "Deleted",
            Self::CustomTime => "CustomTime",
            Self::RetentionMode => "RetentionMode",
            Self::RetentionRetainUntil => "RetentionRetainUntil",
            Self::Md5Hash => "Md5Hash",
            Self::Crc32c => "Crc32c",
        }
    }
    fn fix(self) -> &'static str {
        match self {
            Self::Generation => {
                "Provide the original positive generation below i64::MAX so the allocator can advance safely."
            }
            Self::StorageClassInvalid => {
                "Use an exact supported object storageClass such as STANDARD; empty, lowercase and unknown values are invalid."
            }
            Self::StorageClassUnsupported => {
                "Use an adapter supporting regional/zonal placement; REGIONAL and RAPID cannot be preserved by native publication."
            }
            Self::ContentType
            | Self::ContentEncoding
            | Self::ContentDisposition
            | Self::ContentLanguage
            | Self::CacheControl => {
                "Restore native-compatible fixed metadata: ASCII without control characters and contentLanguage at most 100 characters."
            }
            Self::Metadata => {
                "Restore unique, nonempty custom metadata keys with string values from the original snapshot."
            }
            Self::Acl
            | Self::CustomTime
            | Self::Deleted
            | Self::RetentionMode
            | Self::RetentionRetainUntil => {
                "Use an export/import adapter that preserves these source semantics; this conversion cannot discard active attributes."
            }
            Self::Created | Self::Updated => {
                "Provide a complete source timestamp exact through microseconds; retain pre-epoch instants and do not truncate nonzero fractional digits."
            }
            Self::Md5Hash | Self::Crc32c => {
                "Restore the body and canonical base64 digest together from the same verified snapshot; do not replace the stored digest to conceal corruption."
            }
        }
    }
}

#[derive(Clone, Copy)]
struct ObjectInput<'a> {
    path: &'a Path,
    relative: &'a str,
    bucket: &'a str,
    name: &'a str,
    parent_eligible: bool,
}

/// What one object's attributes parse into; a field is `Err(Reported)` when its blocking
/// finding was reported instead. Fields without a value to keep are `Checked<()>`.
pub(super) struct ObjectChecks {
    generation: Checked<Generation>,
    storage_class: Checked<StorageClass>,
    /// The content fields and custom metadata keys, which publication copies unchanged.
    fixed_metadata: Checked<()>,
    acl: Checked<Option<Vec<AclEntry>>>,
    /// `None` when the source leaves it unset (empty or the zero time).
    custom_time: Checked<Option<String>>,
    created_micros: Checked<i64>,
    updated_micros: Checked<i64>,
    md5: Checked<[u8; 16]>,
    crc32c: Checked<u32>,
}

/// The attributes of an object that every check accepted.
pub(super) struct AcceptedAttributes {
    pub(super) created_micros: i64,
    pub(super) updated_micros: i64,
}

impl ObjectChecks {
    /// The accepted attributes, or `Reported` when this object's checks, or any of `others`
    /// (its remaining checks), reported a finding.
    pub(super) fn accepted(self, others: &[Checked<()>]) -> Checked<AcceptedAttributes> {
        for other in others {
            (*other)?;
        }
        self.generation?;
        self.storage_class?;
        self.fixed_metadata?;
        self.acl?;
        self.custom_time?;
        self.md5?;
        self.crc32c?;
        Ok(AcceptedAttributes {
            created_micros: self.created_micros?,
            updated_micros: self.updated_micros?,
        })
    }
}

/// A body read to its end: its digests, and whether they match the stored checksums.
pub(super) struct VerifiedBody {
    pub(super) hashes: Hashes,
    /// `Reported` when a stored checksum differed.
    pub(super) matches: Checked<()>,
}

fn reject(
    report: &mut Reporter,
    relative: &str,
    field: ObjectField,
    reason: &str,
    original: String,
) -> Result<Reported, ImportFailure> {
    report.block(relative, field.name(), reason, field.fix(), Some(original))
}

/// A source timestamp in Unix microseconds, or the finding that reports why it has none.
fn timestamp(
    report: &mut Reporter,
    relative: &str,
    field: ObjectField,
    value: &str,
) -> Result<Checked<i64>, ImportFailure> {
    Ok(match codec::parse_timestamp(value) {
        Ok(time) => Ok(time),
        Err(reason) => Err(reject(
            report,
            relative,
            field,
            &reason.to_string(),
            value.to_owned(),
        )?),
    })
}

/// Reports every object attribute this conversion cannot preserve and parses the values that
/// later steps need.
pub(super) fn check_object_fields(
    report: &mut Reporter,
    relative: &str,
    attrs: &ObjectAttrs,
) -> Result<ObjectChecks, ImportFailure> {
    // `Generation` refuses zero and below; the allocator needs room above the source's own.
    let generation = match Generation::try_from(attrs.generation)
        .ok()
        .filter(|generation| generation.get() != i64::MAX)
    {
        Some(generation) => Ok(generation),
        None => Err(reject(
            report,
            relative,
            ObjectField::Generation,
            "generation must be positive with room for the allocator to advance",
            attrs.generation.to_string(),
        )?),
    };
    let storage_class = match crate::bucket::validate_storage_class(&attrs.storage_class) {
        Ok(class) => Ok(class),
        Err(error) => {
            let (field, reason) = match error {
                crate::bucket::CatalogError::Metadata(error) if error.unsupported() => (
                    ObjectField::StorageClassUnsupported,
                    "object storageClass requires a region or zone",
                ),
                _ => (
                    ObjectField::StorageClassInvalid,
                    "invalid object storageClass",
                ),
            };
            Err(reject(
                report,
                relative,
                field,
                reason,
                attrs.storage_class.clone(),
            )?)
        }
    };
    let fixed_metadata = check_fixed_metadata(report, relative, attrs)?;
    let acl = match codec::acl(attrs.acl.as_deref(), crate::acl::AclKind::Object) {
        Ok(entries) => Ok(entries),
        Err(error) => Err(reject(
            report,
            relative,
            ObjectField::Acl,
            &error.to_string(),
            serde_json::to_string(&attrs.acl)?,
        )?),
    };
    let custom_time = match codec::is_unset_time(&attrs.custom_time) {
        Ok(true) => Ok(None),
        Ok(false) => Ok(Some(attrs.custom_time.clone())),
        Err(reason) => Err(reject(
            report,
            relative,
            ObjectField::CustomTime,
            &reason.to_string(),
            attrs.custom_time.clone(),
        )?),
    };
    let created_micros = timestamp(report, relative, ObjectField::Created, &attrs.created)?;
    let updated_micros = timestamp(report, relative, ObjectField::Updated, &attrs.updated)?;
    let md5 = match crate::json::decode_hash::<16>(&attrs.md5_hash) {
        Ok(md5) => Ok(md5),
        Err(_) => Err(reject(
            report,
            relative,
            ObjectField::Md5Hash,
            "missing or invalid canonical base64 MD5 (16 bytes required)",
            attrs.md5_hash.clone(),
        )?),
    };
    let crc32c = match crate::json::decode_hash::<4>(&attrs.crc32c) {
        Ok(crc32c) => Ok(u32::from_be_bytes(crc32c)),
        Err(_) => Err(reject(
            report,
            relative,
            ObjectField::Crc32c,
            "missing or invalid canonical base64 CRC32C (4 bytes required)",
            attrs.crc32c.clone(),
        )?),
    };
    Ok(ObjectChecks {
        generation,
        storage_class,
        fixed_metadata,
        acl,
        custom_time,
        created_micros,
        updated_micros,
        md5,
        crc32c,
    })
}

/// Reports every content field and custom metadata key that native publication would refuse.
fn check_fixed_metadata(
    report: &mut Reporter,
    relative: &str,
    attrs: &ObjectAttrs,
) -> Result<Checked<()>, ImportFailure> {
    let mut fixed_metadata = Ok(());
    for (field, key, value) in [
        (ObjectField::ContentType, "contentType", &attrs.content_type),
        (
            ObjectField::ContentEncoding,
            "contentEncoding",
            &attrs.content_encoding,
        ),
        (
            ObjectField::ContentDisposition,
            "contentDisposition",
            &attrs.content_disposition,
        ),
        (
            ObjectField::ContentLanguage,
            "contentLanguage",
            &attrs.content_language,
        ),
        (
            ObjectField::CacheControl,
            "cacheControl",
            &attrs.cache_control,
        ),
    ] {
        if !value.is_empty()
            && let Err(error) = crate::json::validate_fixed_metadata(key, value)
        {
            fixed_metadata = Err(reject(
                report,
                relative,
                field,
                &error.message,
                value.clone(),
            )?);
        }
    }
    if attrs.metadata.as_ref().is_some_and(|m| m.contains_key("")) {
        fixed_metadata = Err(reject(
            report,
            relative,
            ObjectField::Metadata,
            "empty custom metadata key is unsupported",
            String::new(),
        )?);
    }
    Ok(fixed_metadata)
}

/// Reports source semantics that cannot be represented by this live-filesystem adapter.
fn check_unset_semantics(
    report: &mut Reporter,
    relative: &str,
    attrs: &ObjectAttrs,
) -> Result<Checked<()>, ImportFailure> {
    let mut unset = Ok(());
    for (field, value) in [
        (ObjectField::Deleted, &attrs.deleted),
        (
            ObjectField::RetentionRetainUntil,
            &attrs.retention_retain_until,
        ),
    ] {
        let reason = match codec::is_unset_time(value) {
            Ok(true) => continue,
            Ok(false) => "active source semantics are unsupported".to_owned(),
            Err(reason) => reason.to_string(),
        };
        unset = Err(reject(report, relative, field, &reason, value.clone())?);
    }
    if !attrs.retention_mode.is_empty() {
        unset = Err(reject(
            report,
            relative,
            ObjectField::RetentionMode,
            "retention cannot be enforced by this conversion",
            attrs.retention_mode.clone(),
        )?);
    }
    Ok(unset)
}

/// Hashes the whole body and reports stored checksums that do not match it.
pub(super) fn verify_body(
    report: &mut Reporter,
    relative: &str,
    attrs: &ObjectAttrs,
    checks: &ObjectChecks,
    body: &mut BodyStream,
    buffer: &mut [u8; 65_536],
) -> Result<VerifiedBody, ImportFailure> {
    let mut hasher = Hasher::default();
    loop {
        let chunk = body.next_chunk(buffer)?;
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
    }
    if body.read != body.before.size {
        return Err(ImportFailure::SourceChanged {
            fields: vec![ChangedField::Size],
        });
    }
    let hashes = hasher.finish();
    let md5 = if checks.md5.is_ok_and(|expected| expected != hashes.md5) {
        Err(reject(
            report,
            relative,
            ObjectField::Md5Hash,
            "body checksum mismatch",
            attrs.md5_hash.clone(),
        )?)
    } else {
        Ok(())
    };
    let crc32c = if checks
        .crc32c
        .is_ok_and(|expected| expected != hashes.crc32c)
    {
        Err(reject(
            report,
            relative,
            ObjectField::Crc32c,
            "body checksum mismatch",
            attrs.crc32c.clone(),
        )?)
    } else {
        Ok(())
    };
    Ok(VerifiedBody {
        hashes,
        matches: md5.and(crc32c),
    })
}

fn object(
    report: &mut Reporter,
    input: ObjectInput<'_>,
    options: &Options,
    metadata_buffer: &mut SourceMetadataBuffer,
    body_buffer: Option<&mut [u8; 65_536]>,
    xattr: XattrReader,
) -> Result<(), ImportFailure> {
    let ObjectInput {
        path,
        relative,
        bucket,
        name,
        parent_eligible,
    } = input;
    crate::object::name::validate(name).map_err(|_| ImportFailure::corrupt(CorruptField::Name, "unrepresentable object name", "Restore a valid UTF-8 object name of at most 1024 bytes without CR/LF or reserved ACME prefixes."))?;
    if options.source_platform == SourcePlatform::Windows && name.contains('\\') {
        return Err(ImportFailure::corrupt(
            CorruptField::Name,
            "Windows-source backslash mapping is ambiguous on this host",
            "Inspect the source on its native Windows filesystem rather than interpreting a Unix filename backslash as a new path.",
        ));
    }
    let mut body = BodyStream::open(path, options.max_object_bytes)?;
    let bytes = metadata_bytes(&body, options, metadata_buffer, report, relative, xattr)?;
    let attrs: ObjectAttrs = serde_json::from_slice(&bytes).map_err(|e| ImportFailure::corrupt(CorruptField::ObjectAttrs, BackendCause::with_source(format!("invalid object metadata JSON: {e}"), e), "Restore valid ObjectAttrs JSON with exact known field names, unique keys and the pinned field types."))?;
    let checks = check_object_fields(report, relative, &attrs)?;
    let unset = check_unset_semantics(report, relative, &attrs)?;
    let verified = match body_buffer {
        Some(buffer) => Some(verify_body(
            report, relative, &attrs, &checks, &mut body, buffer,
        )?),
        None => None,
    };
    report.summary.bytes_hashed += body.read;
    if let Some(verified) = &verified {
        report.verified_body(relative, verified.hashes);
    }
    match options.metadata_mode {
        MetadataMode::Xattr => {
            if xattr(&body.file, metadata_buffer).map_err(xattr_failure)? != Some(bytes.as_slice())
            {
                return Err(ImportFailure::SourceChanged {
                    fields: vec![ChangedField::Metadata],
                });
            }
        }
        MetadataMode::Sidecar => compare_json_file(&sidecar(path), &bytes)?,
    }
    body.check_unchanged()?;
    report.manifest(
        "object",
        relative,
        &[
            bucket.as_bytes(),
            name.as_bytes(),
            &bytes,
            &body.before.size.to_be_bytes(),
            &body.before.mtime_ns.to_be_bytes(),
        ],
    );
    report.observations(relative, &body.before);
    let body_matches = verified
        .as_ref()
        .map_or(Ok(()), |verified| verified.matches);
    if parent_eligible && let Ok(accepted) = checks.accepted(&[unset, body_matches]) {
        report.summary.eligible_objects += 1;
        report.record(Record::Object {
            path: relative.into(),
            bucket: bucket.into(),
            name: name.into(),
            attributes: Box::new(attrs),
            created_micros: accepted.created_micros,
            updated_micros: accepted.updated_micros,
            deleted_micros: None,
            fingerprint: body.before.clone(),
            verified_hashes: verified.map(|verified| verified.hashes),
            generated: &["native_etag", "metageneration_1"],
            source_file: (report.purpose == ScanPurpose::Publish).then_some(body.file),
            attributes_digest: super::filesystem::sha(&bytes),
        })?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BucketState {
    Eligible,
    Blocked,
    Duplicate,
    Invalid,
}

/// Classifies the root snapshot's directory names before walking, so aliased escapes that
/// decode to one bucket name are all rejected rather than merged.
fn classify_buckets<'a>(
    root_entries: impl Iterator<Item = &'a Path>,
) -> BTreeMap<String, BucketState> {
    let mut names = BTreeMap::<String, usize>::new();
    let mut buckets = BTreeMap::<String, BucketState>::new();
    for path in root_entries {
        if let Some(raw) = path.file_name().and_then(|n| n.to_str()) {
            let state = match codec::path_unescape(raw) {
                Ok(name) if crate::json::validate_bucket_name(&name).is_ok() => {
                    *names.entry(name).or_default() += 1;
                    BucketState::Blocked
                }
                _ => BucketState::Invalid,
            };
            buckets.insert(raw.to_owned(), state);
        }
    }
    for (raw, state) in &mut buckets {
        if *state != BucketState::Invalid
            && let Ok(name) = codec::path_unescape(raw)
            && names.get(&name).is_some_and(|count| *count > 1)
        {
            *state = BucketState::Duplicate;
        }
    }
    buckets
}

pub(super) fn scan(
    options: &Options,
    report: &mut Reporter,
    xattr: XattrReader,
) -> Result<(), ImportFailure> {
    let root = &options.source;
    let walk = Walker::new(root, options.tree_limits()).map_err(walk_failure)?;
    let mut scan = Scan {
        options,
        report,
        xattr,
        buckets: classify_buckets(walk.root_entries()),
        metadata_buffer: SourceMetadataBuffer::default(),
        body_buffer: options.verify_bodies.then(|| Box::new([0; 65_536])),
    };
    for entry in walk {
        let (path, metadata, depth) = entry.map_err(walk_failure)?;
        let report = &mut *scan.report;
        report.summary.entries += 1;
        if depth == 0 && metadata.is_dir() {
            report.summary.buckets += 1;
        }
        if depth > 0
            && metadata.is_file()
            && !(options.metadata_mode == MetadataMode::Sidecar
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(".metadata")))
        {
            report.summary.objects += 1;
        }
        let relative_path = path.strip_prefix(root).map_err(|_| {
            ImportFailure::corrupt(
                CorruptField::Name,
                "path escaped the source root",
                "Supply a separate immutable source root with no escaping paths.",
            )
        })?;
        let Some(relative) = relative_path.to_str() else {
            report.finding(
                "<non-utf8>",
                "name",
                "source path is not UTF-8",
                "Restore representable UTF-8 names from the original export.",
                Severity::Blocking,
                None,
            )?;
            continue;
        };
        let relative = if cfg!(windows) {
            relative.replace('\\', "/")
        } else {
            relative.into()
        };
        let leaf = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
            ImportFailure::corrupt(
                CorruptField::Name,
                "invalid source name",
                "Restore a UTF-8 source entry name.",
            )
        })?;
        let entry = Entry {
            path: &path,
            metadata: &metadata,
            depth,
            relative: &relative,
            leaf,
        };
        if let Err(error) = scan.entry(&entry) {
            scan.report.failure(&relative, &error)?;
        }
    }
    Ok(())
}

/// State shared by every entry of one source walk.
struct Scan<'a> {
    options: &'a Options,
    report: &'a mut Reporter,
    xattr: XattrReader,
    buckets: BTreeMap<String, BucketState>,
    metadata_buffer: SourceMetadataBuffer,
    body_buffer: Option<Box<[u8; 65_536]>>,
}

/// One walked source entry with its UTF-8 path relative to the source root.
struct Entry<'a> {
    path: &'a Path,
    metadata: &'a fs::Metadata,
    depth: usize,
    relative: &'a str,
    leaf: &'a str,
}

impl Scan<'_> {
    fn entry(&mut self, entry: &Entry<'_>) -> Result<(), ImportFailure> {
        let Entry {
            path,
            metadata,
            depth,
            relative,
            leaf,
        } = *entry;
        if !metadata.is_dir() && !metadata.is_file() {
            return Err(ImportFailure::corrupt(
                CorruptField::Entry,
                "symlinks and special source entries are unsupported",
                "Restore regular snapshot files/directories; do not follow symlinks or read devices/FIFOs.",
            ));
        }
        if depth == 0 {
            return self.root_entry(entry);
        }
        let (raw_bucket, name) = relative.split_once('/').ok_or_else(|| {
            ImportFailure::corrupt(
                CorruptField::Name,
                "invalid relative object path",
                "Restore an object below its bucket directory.",
            )
        })?;
        // The root already reports an invalid escape; descendants must still be inspected.
        let bucket = codec::path_unescape(raw_bucket).unwrap_or_else(|_| raw_bucket.to_owned());
        let parent_eligible = self.buckets.get(raw_bucket) == Some(&BucketState::Eligible);
        let report = &mut *self.report;
        if metadata.is_dir() {
            report.summary.directories += 1;
            report.manifest("directory", relative, &[]);
            if parent_eligible {
                report.record(Record::Directory {
                    path: relative.to_owned(),
                })?;
            }
            report.finding(relative, "name", "directory-only candidate: no trailing-slash body or attributes can be recovered", "Compare with the original API inventory if trailing-slash objects matter; no object is synthesized.", Severity::Warning, None)?;
        } else if self.options.metadata_mode == MetadataMode::Sidecar && leaf.ends_with(".metadata")
        {
            check_object_sidecar(path, leaf)?;
        } else {
            if source_tree::is_os_metadata(leaf) {
                let stored = match self.options.metadata_mode {
                    MetadataMode::Sidecar => fs::symlink_metadata(sidecar(path)).is_ok(),
                    MetadataMode::Xattr => {
                        (self.xattr)(&open_regular(path)?, &mut self.metadata_buffer)
                            .map_err(xattr_failure)?
                            .is_some()
                    }
                };
                if !stored {
                    report.summary.objects -= 1;
                    report.summary.skipped_os_metadata += 1;
                    report.manifest("skipped_os_metadata", relative, &[]);
                    return Ok(());
                }
            }
            object(
                report,
                ObjectInput {
                    path,
                    relative,
                    bucket: &bucket,
                    name,
                    parent_eligible,
                },
                self.options,
                &mut self.metadata_buffer,
                self.body_buffer.as_deref_mut(),
                self.xattr,
            )?;
        }
        Ok(())
    }

    fn root_entry(&mut self, entry: &Entry<'_>) -> Result<(), ImportFailure> {
        let Entry {
            metadata,
            relative,
            leaf,
            ..
        } = *entry;
        let root = &self.options.source;
        if metadata.is_dir() {
            self.bucket(entry)?;
        } else if let Some(bucket) = leaf.strip_suffix(".bucketMetadata") {
            if !fs::symlink_metadata(root.join(bucket)).is_ok_and(|m| m.is_dir()) {
                return Err(ImportFailure::corrupt(
                    CorruptField::BucketAttrs,
                    "orphan bucket metadata sidecar",
                    "Restore the matching escaped bucket directory from the same snapshot, or resolve the orphan against the original inventory.",
                ));
            }
        } else if source_tree::is_os_metadata(leaf) {
            self.report.summary.skipped_os_metadata += 1;
            self.report.manifest("skipped_os_metadata", relative, &[]);
        } else {
            return Err(ImportFailure::corrupt(
                CorruptField::Entry,
                "top-level file is not a bucket metadata sidecar",
                "Supply a durable filesystem root containing bucket directories and their exact .bucketMetadata siblings.",
            ));
        }
        Ok(())
    }

    fn bucket(&mut self, entry: &Entry<'_>) -> Result<(), ImportFailure> {
        let Entry {
            metadata,
            relative,
            leaf,
            ..
        } = *entry;
        let root = &self.options.source;
        let report = &mut *self.report;
        if !self.buckets.contains_key(leaf) {
            return Err(ImportFailure::SourceChanged {
                fields: vec![ChangedField::Metadata],
            });
        }
        let name = codec::path_unescape(leaf).map_err(|e| ImportFailure::corrupt(CorruptField::Name, BackendCause::from_error(e), "Restore a valid Go PathEscape bucket directory component from the source snapshot."))?;
        crate::json::validate_bucket_name(&name).map_err(|_| ImportFailure::corrupt(CorruptField::Name, "invalid decoded bucket name", "Use a valid bucket name (3–63 characters for ordinary names, starting/ending with a letter or digit); restore its escaped directory component."))?;
        if self.buckets.get(leaf) == Some(&BucketState::Duplicate) {
            return Err(ImportFailure::corrupt(
                CorruptField::Name,
                "duplicate decoded bucket name",
                "Resolve aliased escaped directory names against the original source inventory; do not merge their contents silently.",
            ));
        }
        let bucket_path = root.join(format!("{leaf}.bucketMetadata"));
        let bytes = match fs::symlink_metadata(&bucket_path) {
            Ok(_) => read_json_file(&bucket_path)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => b"{}".to_vec(),
            Err(e) => return Err(e.into()),
        };
        let attrs: BucketAttrs = serde_json::from_slice(&bytes).map_err(|e| ImportFailure::corrupt(CorruptField::BucketAttrs, BackendCause::with_source(format!("invalid bucket metadata JSON: {e}"), e), "Restore known, unique BucketAttrs fields with the pinned JSON types; an absent sidecar alone means defaults."))?;
        let versioning = if attrs.versioning_enabled == Some(true) {
            Err(report.block(relative, "VersioningEnabled", "unsupported persisted bucket semantics", "The pinned filesystem backend cannot store history; provide a complete memory export for versioned buckets.", None)?)
        } else {
            Ok(())
        };
        let acl = match codec::acl(attrs.acl.as_deref(), crate::acl::AclKind::Bucket) {
            Ok(entries) => Ok(entries),
            Err(error) => Err(report.block(relative, "ACL", error.to_string(), "Restore a supported source ACL without discarding entries or auxiliary identities.", None)?),
        };
        report.manifest("bucket", relative, &[name.as_bytes(), &bytes]);
        if let (Ok(()), Ok(_acl)) = (versioning, acl) {
            self.buckets.insert(leaf.to_owned(), BucketState::Eligible);
            report.summary.eligible_buckets += 1;
            report.record(Record::Bucket {
                path: relative.to_owned(),
                name,
                attributes: attrs,
                created_timestamp: None,
                enable_object_retention: false,
                generated: &[
                    "project_local-project",
                    "native_location",
                    "native_storage_class",
                    "metageneration_1",
                    "creation_time",
                    "no_iam_grants",
                ],
                observed_stat: format!(
                    "report-only modified time: {:?}; source bucket ctime is not creation identity",
                    metadata.modified()?
                ),
                attributes_digest: super::filesystem::sha(&bytes),
            })?;
        }
        Ok(())
    }
}

/// Checks that an object metadata sidecar has its body and is not itself an object name.
fn check_object_sidecar(path: &Path, leaf: &str) -> Result<(), ImportFailure> {
    let base = path.with_file_name(leaf.strip_suffix(".metadata").ok_or_else(|| {
        ImportFailure::corrupt(
            CorruptField::Metadata,
            "invalid sidecar suffix",
            "Restore the exact <object>.metadata neighbor name.",
        )
    })?);
    if !fs::symlink_metadata(&base).is_ok_and(|m| m.is_file()) {
        return Err(ImportFailure::corrupt(
            CorruptField::Metadata,
            "orphan or ambiguous object metadata sidecar; body is missing",
            "Restore the matching body from the same snapshot; do not synthesize an empty object.",
        ));
    }
    if fs::symlink_metadata(sidecar(path)).is_ok() {
        return Err(ImportFailure::corrupt(
            CorruptField::Metadata,
            "ambiguous .metadata object/sidecar suffix collision",
            "Resolve the .metadata suffix collision against the source inventory before conversion.",
        ));
    }
    Ok(())
}

fn walk_failure(error: WalkError) -> ImportFailure {
    match error {
        WalkError::Io(error) => ImportFailure::Io(error),
        WalkError::EntryLimit { .. } => ImportFailure::corrupt(
            CorruptField::Limits,
            "source entry limit exceeded",
            "Raise --max-entries, or split the source to fit the entry limit, which counts sidecars, before retrying.",
        ),
        WalkError::DepthLimit { limit } => ImportFailure::corrupt(
            CorruptField::Limits,
            "source depth limit exceeded",
            format!(
                "Raise --max-depth, or provide source directories within {limit} levels below a bucket; do not silently flatten object names."
            ),
        ),
        WalkError::NotDirectory => ImportFailure::corrupt(
            CorruptField::Entry,
            "source root is not a directory",
            "Restore an accessible source directory, not a symlink or special entry.",
        ),
    }
}
