//! Bounded, read-only decoding of complete quiesced source memory captures.

use super::{
    BodyStream, Fingerprint, ImportFailure, Options, Record, Reporter, ScanPurpose,
    codec::{self, BucketAttrs, ObjectAttrs},
    reader,
};
use crate::{fsutil, hashing::encode_lower};
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

const LINE_CAP: u64 = 128 * 1024;

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
    Bucket {
        name: String,
        created: String,
        attributes: BucketAttrs,
        enable_object_retention: bool,
    },
    Object {
        bucket: String,
        name: String,
        membership: Membership,
        blob: String,
        size: u64,
        attributes: Box<ObjectAttrs>,
    },
    Complete {
        buckets: u64,
        generations: u64,
        bytes: u64,
        manifest_sha256: String,
    },
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Membership {
    Live,
    Archived,
}

fn corrupt(reason: impl Into<String>) -> ImportFailure {
    ImportFailure::corrupt(
        "snapshot",
        reason,
        "Retain the source and export a complete immutable capture under the source writer barrier.",
    )
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn line(input: &mut BufReader<File>) -> Result<Vec<u8>, ImportFailure> {
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

struct Capture<'a> {
    options: &'a Options,
    report: &'a mut Reporter,
    blobs: File,
    bucket: String,
    retention_enabled: bool,
    previous: Option<(String, Membership, i64)>,
    generations_for_name: BTreeSet<i64>,
    buckets: u64,
    generations: u64,
    bytes: u64,
}

impl Capture<'_> {
    fn bucket(
        &mut self,
        name: String,
        created: &str,
        attributes: BucketAttrs,
        retention: crate::object::retention::BucketRetention,
        raw: &[u8],
    ) -> Result<(), ImportFailure> {
        crate::json::validate_bucket_name(&name).map_err(|error| corrupt(error.message))?;
        if name <= self.bucket {
            return Err(corrupt("bucket rows must be unique and in lexical order"));
        }
        codec::parse_bucket_timestamp(created).map_err(corrupt)?;
        codec::acl(attributes.acl.as_deref(), crate::acl::AclKind::Bucket)?;
        self.buckets += 1;
        self.report.summary.buckets += 1;
        self.report.summary.eligible_buckets += 1;
        self.report.manifest("snapshot_bucket", &name, &[raw]);
        let retention = retention == crate::object::retention::BucketRetention::Enabled;
        self.report.record(Record::Bucket {
            path: format!("bucket:{name}"),
            name: name.clone(),
            attributes,
            created_timestamp: Some(created.into()),
            enable_object_retention: retention,
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
        })?;
        self.bucket = name;
        self.retention_enabled = retention;
        self.previous = None;
        self.generations_for_name.clear();
        Ok(())
    }

    fn order(
        &mut self,
        bucket: &str,
        name: &str,
        membership: Membership,
        generation: i64,
    ) -> Result<(), ImportFailure> {
        crate::json::validate_object_name(name).map_err(|error| corrupt(error.message))?;
        if bucket != self.bucket || bucket.is_empty() {
            return Err(corrupt("object must follow its declared bucket"));
        }
        if let Some((previous, member, previous_generation)) = &self.previous
            && (name < previous.as_str()
                || (name == previous
                    && (*member == Membership::Live
                        || generation == *previous_generation
                        || (membership == Membership::Archived
                            && generation <= *previous_generation))))
        {
            return Err(corrupt(
                "object names, archive generations or live membership are duplicated or out of order",
            ));
        }
        if self
            .previous
            .as_ref()
            .is_none_or(|(previous, _, _)| previous != name)
        {
            self.generations_for_name.clear();
        }
        if !self.generations_for_name.insert(generation) {
            return Err(corrupt("duplicate generation in the same source object"));
        }
        self.previous = Some((name.into(), membership, generation));
        Ok(())
    }

    fn object(&mut self, object: SnapshotObject, raw: &[u8]) -> Result<(), ImportFailure> {
        let SnapshotObject {
            bucket,
            name,
            membership,
            blob,
            size,
            attributes,
        } = object;
        self.order(&bucket, &name, membership, attributes.generation)?;
        let ordinal = self
            .generations
            .checked_add(1)
            .ok_or_else(|| corrupt("generation count overflow"))?;
        if blob != format!("{ordinal:016x}") {
            return Err(corrupt(
                "body filename must be the next opaque hexadecimal ordinal",
            ));
        }
        let before = self.report.summary.blocking_findings;
        let checks = reader::check_object_fields(self.report, &blob, &attributes)?;
        let retention = codec::retention(&attributes)?;
        if retention.is_some() && !self.retention_enabled {
            return Err(corrupt(
                "retention configuration requires declared derived bucket enablement",
            ));
        }
        let deleted = match membership {
            Membership::Live => {
                if !codec::is_unset_time(&attributes.deleted).map_err(corrupt)? {
                    return Err(corrupt("live member carries a nonzero Deleted timestamp"));
                }
                None
            }
            Membership::Archived => {
                Some(codec::parse_timestamp(&attributes.deleted).map_err(corrupt)?)
            }
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
        let hashes = if self.options.verify_bodies {
            let hashes = reader::verify_body(
                self.report,
                &blob,
                &attributes,
                &checks,
                &mut body,
                &mut [0; 65_536],
            )?;
            self.report.summary.bytes_hashed += size;
            self.report.verified_body(&blob, hashes);
            Some(hashes)
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
        if self.report.summary.blocking_findings == before {
            self.report.summary.eligible_objects += 1;
            self.report.record(Record::Object {
                path: blob,
                bucket,
                name,
                attributes,
                created_micros: checks.times[0],
                updated_micros: checks.times[1],
                deleted_micros: deleted,
                fingerprint,
                verified_hashes: hashes,
                generated: &["native_etag", "metageneration_1"],
                source_file: (self.report.purpose == ScanPurpose::Publish)
                    .then(|| body.into_file()),
                attributes_digest: super::filesystem::sha(raw),
            })?;
        }
        Ok(())
    }

    fn check_blobs(&self) -> Result<(), ImportFailure> {
        let mut after = None;
        let mut count = 0u64;
        loop {
            let page = fsutil::directory_names_after(&self.blobs, after.as_deref(), 256)?;
            if page.is_empty() {
                break;
            }
            for name in page {
                count += 1;
                if name != std::ffi::OsStr::new(&format!("{count:016x}"))
                    || count > self.generations
                {
                    return Err(corrupt("unknown, duplicate or unreferenced body entry"));
                }
                after = Some(name);
            }
        }
        if count != self.generations {
            return Err(corrupt("missing snapshot body entries"));
        }
        Ok(())
    }
}

struct SnapshotObject {
    bucket: String,
    name: String,
    membership: Membership,
    blob: String,
    size: u64,
    attributes: Box<ObjectAttrs>,
}

fn check_attribute_cap(bytes: &[u8]) -> Result<(), ImportFailure> {
    #[derive(Deserialize)]
    struct Attributes {
        attributes: Box<serde_json::value::RawValue>,
    }
    let raw: Attributes = serde_json::from_slice(bytes)?;
    if raw.attributes.get().len() > fsutil::SOURCE_METADATA_CAP {
        return Err(corrupt("source attributes exceed 64 KiB"));
    }
    Ok(())
}

struct Snapshot {
    root: File,
    input: BufReader<File>,
    before: Fingerprint,
    hash: Context,
    blobs: File,
}

fn open(options: &Options, report: &mut Reporter) -> Result<Snapshot, ImportFailure> {
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
    let file = fsutil::open_regular_at(&root, "snapshot.jsonl".as_ref())?;
    let before = Fingerprint::of_file(&file)?;
    let blobs = fsutil::directory_at(&root, "blobs".as_ref(), fsutil::DirectoryOpen::Existing)?;
    let mut input = BufReader::new(file);
    let header = line(&mut input)?;
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
            && hex(&capture_id, 32) => {}
        _ => return Err(corrupt("unsupported or unquiesced source snapshot header")),
    }
    report.manifest("snapshot_header", "snapshot.jsonl", &[&header]);
    let mut hash = Context::new(&SHA256);
    hash.update(&header);
    Ok(Snapshot {
        root,
        input,
        before,
        hash,
        blobs,
    })
}

pub(super) fn scan(options: &Options, report: &mut Reporter) -> Result<(), ImportFailure> {
    let Snapshot {
        root,
        mut input,
        before,
        mut hash,
        blobs,
    } = open(options, report)?;
    let mut capture = Capture {
        options,
        report,
        blobs,
        bucket: String::new(),
        retention_enabled: false,
        previous: None,
        generations_for_name: BTreeSet::new(),
        buckets: 0,
        generations: 0,
        bytes: 0,
    };
    loop {
        let bytes = line(&mut input)?;
        let row: Row = serde_json::from_slice(&bytes)?;
        if matches!(row, Row::Bucket { .. } | Row::Object { .. }) {
            check_attribute_cap(&bytes)?;
        }
        if let Row::Complete {
            buckets,
            generations,
            bytes: total,
            manifest_sha256,
        } = row
        {
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
        capture.report.summary.entries += 1;
        if capture.report.summary.entries > options.max_entries as u64 {
            return Err(corrupt("snapshot entry limit exceeded"));
        }
        hash.update(&bytes);
        match row {
            Row::Bucket {
                name,
                created,
                attributes,
                enable_object_retention,
            } => capture.bucket(
                name,
                &created,
                attributes,
                if enable_object_retention {
                    crate::object::retention::BucketRetention::Enabled
                } else {
                    crate::object::retention::BucketRetention::Disabled
                },
                &bytes,
            )?,
            Row::Object {
                bucket,
                name,
                membership,
                blob,
                size,
                attributes,
            } => capture.object(
                SnapshotObject {
                    bucket,
                    name,
                    membership,
                    blob,
                    size,
                    attributes,
                },
                &bytes,
            )?,
            _ => return Err(corrupt("unexpected snapshot record")),
        }
    }
    capture.check_blobs()?;
    before.check(&Fingerprint::of_file(input.get_ref())?)?;
    before.check(&Fingerprint::of_file(&fsutil::open_regular_at(
        &root,
        "snapshot.jsonl".as_ref(),
    )?)?)?;
    capture.report.summary.original_names_verified = true;
    Ok(())
}
