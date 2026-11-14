//! Bounded, read-only decoding of complete quiesced source memory captures.

use super::{
    BodyStream, BucketRecord, CorruptField, Fingerprint, ImportFailure, ObjectRecord, Options,
    Record, Reporter, ScanPurpose,
    codec::{self, BucketAttrs, ObjectAttrs},
    reader::{self, PinnedSource},
    spec::BucketSpec,
};
use crate::{
    bucket::name::BucketName, fsutil, hashing::encode_lower, object::name::ObjectName,
    storage::failure::BackendCause,
};
use ring::digest::{Context, SHA256};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader, Read},
};

// Snapshot import is Unix-only; `Options::validate` rejects it elsewhere and its own test
// covers that.
#[cfg(test)]
#[cfg(unix)]
mod tests;

/// A line holds one source attribute document plus the wrapper fields around it.
const LINE_CAP: u64 = 2 * fsutil::SOURCE_METADATA_CAP as u64;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Row {
    Snapshot {
        version: u32,
        source_revision: String,
        capture_id: String,
        quiesced: bool,
        backend: String,
        original_names_verified: bool,
    },
    Bucket(BucketRow),
    Object(ObjectRow),
    Complete {
        buckets: u64,
        generations: u64,
        bytes: u64,
        manifest_sha256: String,
    },
}

/// A tagged row's newtype variant reads its struct from the fields after the tag, so the struct
/// itself refuses the fields it does not declare.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BucketRow {
    name: String,
    created: String,
    attributes: BucketAttrs,
    enable_object_retention: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectRow {
    bucket: String,
    name: String,
    membership: Membership,
    blob: String,
    size: u64,
    attributes: Box<ObjectAttrs>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Membership {
    Live,
    Archived,
}

fn corrupt(reason: impl Into<BackendCause>) -> ImportFailure {
    ImportFailure::corrupt(
        CorruptField::Snapshot,
        reason,
        "Retain the source and export a complete immutable capture under the source writer barrier.",
    )
}

fn line(input: &mut impl BufRead) -> Result<Vec<u8>, ImportFailure> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(LINE_CAP + 1)
        .read_until(b'\n', &mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > LINE_CAP || bytes.last() != Some(&b'\n') {
        return Err(corrupt("missing, incomplete or oversized snapshot line"));
    }
    Ok(bytes)
}

/// The bucket whose objects the capture is reading.
struct CurrentBucket {
    name: BucketName,
    object_retention_enabled: bool,
    last: Option<LastObject>,
    /// Generations already read for `last`'s name: a live generation must not repeat an
    /// archived one.
    generations: BTreeSet<i64>,
}

/// The previous object row of the current bucket, which the next row must follow.
struct LastObject {
    name: ObjectName,
    membership: Membership,
    generation: i64,
}

/// Where an object row belongs: its bucket, and whether that bucket enables object retention.
struct Placement {
    bucket: BucketName,
    object_retention_enabled: bool,
}

struct Capture<'a> {
    options: &'a Options,
    report: &'a mut Reporter,
    blobs: File,
    current: Option<CurrentBucket>,
    buckets: u64,
    generations: u64,
    bytes: u64,
}

impl Capture<'_> {
    /// Counts one data row against the entry limit and adds it to the capture digest.
    fn admit(&mut self, hash: &mut Context, row: &[u8]) -> Result<(), ImportFailure> {
        self.report.summary.entries += 1;
        if self
            .options
            .max_entries
            .is_some_and(|limit| self.report.summary.entries > limit.get() as u64)
        {
            return Err(ImportFailure::corrupt(
                CorruptField::Limits,
                "snapshot entry limit exceeded",
                "Raise --max-entries to at least the number of snapshot rows, then retry.",
            ));
        }
        hash.update(row);
        Ok(())
    }

    fn bucket(&mut self, row: BucketRow, raw: &[u8]) -> Result<(), ImportFailure> {
        let BucketRow {
            name,
            created,
            attributes,
            enable_object_retention,
        } = row;
        let name = BucketName::try_from(name).map_err(|_| corrupt("Invalid bucket name"))?;
        if self
            .current
            .as_ref()
            .is_some_and(|current| name <= current.name)
        {
            return Err(corrupt("bucket rows must be unique and in lexical order"));
        }
        let spec = BucketSpec {
            created: Some(
                codec::parse_bucket_timestamp(&created)
                    .map_err(|error| corrupt(BackendCause::from_error(error)))?,
            ),
            acl: codec::acl(attributes.acl.as_deref(), crate::acl::AclKind::Bucket)
                .map_err(|error| corrupt(BackendCause::from_error(error)))?,
        };
        self.buckets += 1;
        self.report.summary.buckets += 1;
        self.report.summary.eligible_buckets += 1;
        self.report
            .manifest("snapshot_bucket", name.as_str(), &[raw]);
        self.report.record(Record::Bucket(Box::new(BucketRecord {
            path: format!("bucket:{name}"),
            name: name.clone(),
            attributes,
            created_timestamp: Some(created),
            enable_object_retention,
            generated: &[
                "project_local-project",
                "native_location",
                "native_storage_class",
                "metageneration_1",
                "object_retention_enablement_derived",
                "no_iam_grants",
            ],
            observed_stat: "creation identity from the source memory bucket".into(),
            attributes_digest: super::filesystem::sha(raw),
            spec,
        })))?;
        self.current = Some(CurrentBucket {
            name,
            object_retention_enabled: enable_object_retention,
            last: None,
            generations: BTreeSet::new(),
        });
        Ok(())
    }

    /// Checks the row's place after the previous one; returns the bucket it belongs to.
    fn order(
        &mut self,
        bucket: &str,
        name: &ObjectName,
        membership: Membership,
        generation: i64,
    ) -> Result<Placement, ImportFailure> {
        let Some(current) = self
            .current
            .as_mut()
            .filter(|current| current.name.as_str() == bucket)
        else {
            return Err(corrupt("object must follow its declared bucket"));
        };
        if let Some(last) = &current.last
            && (*name < last.name
                || (*name == last.name
                    && (last.membership == Membership::Live
                        || generation == last.generation
                        || (membership == Membership::Archived && generation <= last.generation))))
        {
            return Err(corrupt(
                "object names, archive generations or live membership are duplicated or out of order",
            ));
        }
        if current.last.as_ref().is_none_or(|last| last.name != *name) {
            current.generations.clear();
        }
        if !current.generations.insert(generation) {
            return Err(corrupt("duplicate generation in the same source object"));
        }
        current.last = Some(LastObject {
            name: name.clone(),
            membership,
            generation,
        });
        Ok(Placement {
            bucket: current.name.clone(),
            object_retention_enabled: current.object_retention_enabled,
        })
    }

    fn object(&mut self, row: ObjectRow, raw: &[u8]) -> Result<(), ImportFailure> {
        let ObjectRow {
            bucket,
            name,
            membership,
            blob,
            size,
            attributes,
        } = row;
        let name = ObjectName::try_from(name).map_err(|_| corrupt("Invalid object name"))?;
        let placement = self.order(&bucket, &name, membership, attributes.generation)?;
        let ordinal = self
            .generations
            .checked_add(1)
            .ok_or_else(|| corrupt("generation count overflow"))?;
        if blob != format!("{ordinal:016x}") {
            return Err(corrupt(
                "body filename must be the next opaque hexadecimal ordinal",
            ));
        }
        let checks = reader::check_object_fields(self.report, &blob, &attributes)?;
        let retention = codec::retention(&attributes)
            .map_err(|error| corrupt(BackendCause::from_error(error)))?;
        if retention.is_some() && !placement.object_retention_enabled {
            return Err(corrupt(
                "retention configuration requires declared derived bucket enablement",
            ));
        }
        let deleted = match membership {
            Membership::Live => {
                if !codec::is_unset_time(&attributes.deleted)
                    .map_err(|error| corrupt(BackendCause::from_error(error)))?
                {
                    return Err(corrupt("live member carries a nonzero Deleted timestamp"));
                }
                None
            }
            Membership::Archived => Some(
                codec::parse_timestamp(&attributes.deleted)
                    .map_err(|error| corrupt(BackendCause::from_error(error)))?,
            ),
        };
        let file = fsutil::open_regular_at(&self.blobs, blob.as_ref())?;
        let mut body = BodyStream::from_file(
            file,
            &self.options.source.join("blobs").join(&blob),
            self.options.max_object_bytes,
        )?;
        if body.fingerprint().size != size {
            return Err(corrupt("snapshot size differs from the selected body"));
        }
        let verified = if self.options.verify_bodies {
            let verified = reader::verify_body(
                self.report,
                &blob,
                &attributes,
                &checks,
                &mut body,
                &mut [0; 65_536],
            )?;
            self.report.summary.bytes_hashed += size;
            self.report.verified_body(&blob, verified.hashes);
            Some(verified)
        } else {
            None
        };
        body.check_unchanged()?;
        let fingerprint = body.fingerprint().clone();
        self.report.observations(&blob, &fingerprint);
        self.report.manifest(
            "snapshot_object",
            &blob,
            &[
                raw,
                &fingerprint.size.to_be_bytes(),
                &fingerprint.mtime_ns.to_be_bytes(),
            ],
        );
        self.generations = ordinal;
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or_else(|| corrupt("body byte total overflow"))?;
        self.report.summary.objects += 1;
        let body_matches = verified
            .as_ref()
            .map_or(Ok(()), |verified| verified.matches);
        if let Ok(accepted) = checks.accepted(retention, &[body_matches]) {
            self.report.summary.eligible_objects += 1;
            self.report.record(Record::Object(Box::new(ObjectRecord {
                path: blob,
                bucket: placement.bucket,
                name,
                attributes: *attributes,
                created_micros: accepted.created_micros,
                updated_micros: accepted.updated_micros,
                deleted_micros: deleted,
                fingerprint,
                verified_hashes: verified.map(|verified| verified.hashes),
                generated: &["native_etag", "metageneration_1"],
                source_file: (self.report.purpose == ScanPurpose::Publish)
                    .then(|| body.into_file()),
                attributes_digest: super::filesystem::sha(raw),
                spec: accepted.spec,
            })))?;
        }
        Ok(())
    }

    fn check_blobs(&self) -> Result<(), ImportFailure> {
        let mut names = fsutil::DirectoryNames::default();
        let mut count = 0u64;
        while let Some(name) = names.next_name(&self.blobs, usize::MAX)? {
            count += 1;
            if name != std::ffi::OsStr::new(&format!("{count:016x}")) || count > self.generations {
                return Err(corrupt("unknown, duplicate or unreferenced body entry"));
            }
        }
        if count != self.generations {
            return Err(corrupt("missing snapshot body entries"));
        }
        Ok(())
    }
}

fn check_attribute_cap(bytes: &[u8]) -> Result<(), ImportFailure> {
    #[derive(Deserialize)]
    struct Attributes {
        attributes: Box<serde_json::value::RawValue>,
    }
    let raw: Attributes = serde_json::from_slice(bytes)?;
    if raw.attributes.get().len() > fsutil::SOURCE_METADATA_CAP {
        return Err(corrupt("source attributes exceed 16 MiB"));
    }
    Ok(())
}

/// An opened capture: its root and body directories, and its row file, pinned until the scan
/// rechecks it.
struct Snapshot {
    root: File,
    rows: PinnedSource,
    blobs: File,
}

fn open(options: &Options) -> Result<Snapshot, ImportFailure> {
    let root = fsutil::open_directory_nofollow(&options.source)?;
    let names = fsutil::directory_names_after(&root, None, 256)?;
    if names
        != [
            std::ffi::OsString::from("blobs"),
            std::ffi::OsString::from("snapshot.jsonl"),
        ]
    {
        return Err(corrupt(
            "snapshot root must contain exactly blobs/ and snapshot.jsonl",
        ));
    }
    let rows = PinnedSource::pin(fsutil::open_regular_at(&root, "snapshot.jsonl".as_ref())?)?;
    let blobs = fsutil::directory_at(&root, "blobs".as_ref(), fsutil::DirectoryOpen::Existing)?;
    Ok(Snapshot { root, rows, blobs })
}

/// Reads and checks the header row, which starts the capture digest.
fn header(input: &mut impl BufRead, report: &mut Reporter) -> Result<Context, ImportFailure> {
    let header = line(input)?;
    match serde_json::from_slice::<Row>(&header)? {
        Row::Snapshot {
            version: 1,
            source_revision,
            capture_id,
            quiesced: true,
            backend,
            original_names_verified: true,
        } if source_revision == super::SOURCE_REVISION
            && backend == "memory"
            && crate::hex::is_lower(&capture_id, 32) => {}
        Row::Snapshot { .. } | Row::Bucket(_) | Row::Object(_) | Row::Complete { .. } => {
            return Err(corrupt("unsupported or unquiesced source snapshot header"));
        }
    }
    report.manifest("snapshot_header", "snapshot.jsonl", &[&header]);
    let mut hash = Context::new(&SHA256);
    hash.update(&header);
    Ok(hash)
}

pub(super) fn scan(options: &Options, report: &mut Reporter) -> Result<(), ImportFailure> {
    let Snapshot { root, rows, blobs } = open(options)?;
    let mut input = BufReader::new(rows.file());
    let mut hash = header(&mut input, report)?;
    let mut capture = Capture {
        options,
        report,
        blobs,
        current: None,
        buckets: 0,
        generations: 0,
        bytes: 0,
    };
    loop {
        let bytes = line(&mut input)?;
        match serde_json::from_slice::<Row>(&bytes)? {
            Row::Complete {
                buckets,
                generations,
                bytes: total,
                manifest_sha256,
            } => {
                if (buckets, generations, total)
                    != (capture.buckets, capture.generations, capture.bytes)
                    || manifest_sha256 != encode_lower(hash.finish().as_ref())
                    || !input.fill_buf()?.is_empty()
                {
                    return Err(corrupt(
                        "snapshot completion counts, digest or EOF do not match",
                    ));
                }
                break;
            }
            // A second header still counts against the limit, so a capture past it names the
            // option to raise first.
            Row::Snapshot { .. } => {
                capture.admit(&mut hash, &bytes)?;
                return Err(corrupt("unexpected snapshot record"));
            }
            Row::Bucket(row) => {
                check_attribute_cap(&bytes)?;
                capture.admit(&mut hash, &bytes)?;
                capture.bucket(row, &bytes)?;
            }
            Row::Object(row) => {
                check_attribute_cap(&bytes)?;
                capture.admit(&mut hash, &bytes)?;
                capture.object(row, &bytes)?;
            }
        }
    }
    capture.check_blobs()?;
    rows.verify_unchanged(|| {
        Fingerprint::of_file(&fsutil::open_regular_at(&root, "snapshot.jsonl".as_ref())?)
    })?;
    capture.report.summary.original_names_verified = true;
    Ok(())
}
