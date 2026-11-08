#![deny(clippy::unwrap_used, clippy::expect_used)]

//! Versioned bucket initialization with complete validation before create-missing.

use crate::{
    auth::Policy,
    bounded_file::{self, ReadError},
    bucket::BucketRecord,
    clock::Clock,
    config::CompatProfile,
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    json::{parse_insert_with_profile, validate_project},
    storage::{
        Catalog,
        admin::{AdminError, Origin},
        failure::MemoryBudget,
    },
};
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, Visitor},
};
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    path::Path,
    time::SystemTime,
};

mod plan;
mod policy;
mod written;

use written::{Member, Written};

/// Leaves room beside one entry of the largest size, which a smaller file limit would refuse first.
pub(crate) const FILE_LIMIT: usize = 2 * ENTRY_LIMIT;
/// What each allowed bucket adds to a file: a fixed size would otherwise refuse a file long before
/// it reached the configured bucket limit.
const FILE_BYTES_PER_BUCKET: usize = 1024;
/// The whole file is read into memory and parsed, so even without a bucket limit a mistaken path
/// to some huge file must fail here. A million buckets of 1 KiB each still fit.
const UNLIMITED_FILE_LIMIT: usize = 1024 * 1024 * 1024;
pub(crate) const ENTRY_LIMIT: usize = crate::json::RESOURCE_BODY_LIMIT;
pub(crate) const DIAGNOSTIC_LIMIT: usize = 256;
pub(crate) const DIAGNOSTIC_BYTES_LIMIT: usize = 64 * 1024;
const ARRAY_DIAGNOSTIC_LIMIT: usize = 4;
const ROOT_DIAGNOSTIC_LIMIT: usize = 16;
const ROOT_BYTES_LIMIT: usize = 8 * 1024;
const SUMMARY_BYTES_RESERVE: usize = 12 * 1024;
const ENTRY_DIAGNOSTIC_LIMIT: usize = DIAGNOSTIC_LIMIT - ROOT_DIAGNOSTIC_LIMIT - 1;
const ENTRY_BYTES_LIMIT: usize = DIAGNOSTIC_BYTES_LIMIT - ROOT_BYTES_LIMIT - SUMMARY_BYTES_RESERVE;
const RECORD_BYTES_LIMIT: usize = 2 * 1024;
const GROUP_LIMIT: usize = 1024;
const SHORT_KEY_BYTES: usize = 256;
const REASON_COUNT: usize = 8;
const REASON_LABEL_BYTES: usize = "unsupported-field".len();
const fn decimal_width(mut value: usize) -> usize {
    let mut width = 1;
    while value >= 10 {
        value /= 10;
        width += 1;
    }
    width
}
const MAX_REASON_BYTES: usize = REASON_COUNT * (REASON_LABEL_BYTES + decimal_width(usize::MAX) + 3);
const SUMMARY_TEXT_BYTES: usize = 512;
const SUMMARY_ENVELOPE_BYTES: usize = 512;
const MORE_RANGES_BYTES: usize = " and ".len() + decimal_width(usize::MAX) + " more ranges".len();
/// The bucket limit is configured, so no count of ranges can be proven to fit: `entry_ranges`
/// stops at this many bytes instead, which keeps the summary inside its reservation at any limit.
const RANGE_BYTES_LIMIT: usize = SUMMARY_BYTES_RESERVE
    - SUMMARY_ENVELOPE_BYTES
    - SUMMARY_TEXT_BYTES
    - 3 * MAX_REASON_BYTES
    - MORE_RANGES_BYTES;
const MAX_SUMMARY_RECORD_BYTES: usize =
    RANGE_BYTES_LIMIT + MORE_RANGES_BYTES + 3 * MAX_REASON_BYTES + SUMMARY_TEXT_BYTES;
const _: () = assert!(MAX_SUMMARY_RECORD_BYTES + SUMMARY_ENVELOPE_BYTES <= SUMMARY_BYTES_RESERVE);
/// Alternating failures make the most ranges, and all of them fit up to this many buckets, so only
/// a larger file ever shortens the list.
const FULLY_LISTED_BUCKETS: usize = 1000;
const _: () = assert!(
    FULLY_LISTED_BUCKETS.div_ceil(2)
        * ("buckets[".len() + 2 * decimal_width(FULLY_LISTED_BUCKETS - 1) + "..], ".len())
        <= RANGE_BYTES_LIMIT
);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reason {
    Shape,
    Unknown,
    Unsupported,
    Repeated,
    Duplicate,
    Project,
    Native,
    Limit,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RootKey {
    Version,
    Project,
    Buckets,
    SeedProject,
}
impl RootKey {
    fn parse(key: &str) -> Option<Self> {
        match key {
            "version" => Some(Self::Version),
            "project" => Some(Self::Project),
            "buckets" => Some(Self::Buckets),
            "seed_project" => Some(Self::SeedProject),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct RetentionLimit {
    records: usize,
    bytes: usize,
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    Extra,
    FirstKey,
    Structural,
}
impl Reason {
    fn label(self) -> &'static str {
        match self {
            Self::Shape => "structure",
            Self::Unknown => "unknown-field",
            Self::Unsupported => "unsupported-field",
            Self::Repeated => "repeated-member",
            Self::Duplicate => "duplicate-name",
            Self::Project => "project",
            Self::Native => "native-value",
            Self::Limit => "limit",
        }
    }
}

struct Retained {
    diagnostic: Diagnostic,
    bytes: usize,
    priority: Priority,
    reason: Reason,
    group: [u8; 32],
}

/// What the diagnostics say about a failing entry. Passing entries take no space, so a clean
/// file of any size costs nothing here, and a failing entry costs far less than parsing it did.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryReport {
    /// Failed, but only the summary mentions it so far.
    Counted,
    Detailed,
}

/// How many of `entries` a file with this limit gets to declare; the rest are not inspected.
fn inspected_entries(bucket_limit: Option<NonZeroUsize>, entries: usize) -> usize {
    bucket_limit.map_or(entries, |limit| entries.min(limit.get()))
}

struct Collected {
    bucket_limit: Option<NonZeroUsize>,
    root: Vec<Retained>,
    entries: Vec<Retained>,
    root_bytes: usize,
    entry_bytes: usize,
    total: usize,
    entry_reports: BTreeMap<usize, EntryReport>,
    root_seen: BTreeSet<(RootKey, Reason)>,
    root_unknown_seen: BTreeSet<[u8; 32]>,
    groups: BTreeMap<([u8; 32], Reason), u8>,
    reasons: BTreeMap<&'static str, usize>,
    first_reasons: BTreeMap<&'static str, usize>,
    root_reasons: BTreeMap<&'static str, usize>,
}

impl Collected {
    fn new(bucket_limit: Option<NonZeroUsize>) -> Self {
        Self {
            bucket_limit,
            root: Vec::new(),
            entries: Vec::new(),
            root_bytes: 0,
            entry_bytes: 0,
            total: 0,
            entry_reports: BTreeMap::new(),
            root_seen: BTreeSet::new(),
            root_unknown_seen: BTreeSet::new(),
            groups: BTreeMap::new(),
            reasons: BTreeMap::new(),
            first_reasons: BTreeMap::new(),
            root_reasons: BTreeMap::new(),
        }
    }

    fn count(&self) -> usize {
        self.total
    }
    fn is_err(&self) -> bool {
        self.total != 0
    }

    fn record(&mut self, reason: Reason, key: String, fix: impl Into<String>) {
        self.total += 1;
        *self.reasons.entry(reason.label()).or_default() += 1;
        let entry = key
            .strip_prefix("buckets[")
            .and_then(|suffix| suffix.split_once(']'))
            .and_then(|(index, _)| index.parse::<usize>().ok())
            .filter(|index| self.bucket_limit.is_none_or(|limit| *index < limit.get()));
        let root_key = RootKey::parse(&key);
        let group = (group_digest(&key), reason);
        let priority = if let Some(index) = entry {
            match self.entry_reports.get(&index) {
                None => {
                    self.entry_reports.insert(index, EntryReport::Counted);
                    *self.first_reasons.entry(reason.label()).or_default() += 1;
                    Priority::FirstKey
                }
                Some(EntryReport::Counted) => Priority::FirstKey,
                Some(EntryReport::Detailed) => Priority::Extra,
            }
        } else {
            *self.root_reasons.entry(reason.label()).or_default() += 1;
            if root_key.is_some_and(|root| !self.root_seen.contains(&(root, reason))) {
                Priority::Structural
            } else if reason == Reason::Unknown && !self.root_unknown_seen.contains(&group.0) {
                Priority::FirstKey
            } else {
                Priority::Extra
            }
        };
        if self
            .groups
            .get(&group)
            .is_some_and(|count| usize::from(*count) >= ARRAY_DIAGNOSTIC_LIMIT)
        {
            return;
        }
        if !self.groups.contains_key(&group)
            && self.groups.len() >= GROUP_LIMIT
            && priority == Priority::Extra
        {
            return;
        }
        let mut diagnostic = Diagnostic::new(DiagnosticSource::Initializer, key, fix);
        let mut bytes = if diagnostic.key.len() + diagnostic.fix.len() <= RECORD_BYTES_LIMIT {
            diagnostic_bytes(&diagnostic)
        } else {
            usize::MAX
        };
        if bytes > RECORD_BYTES_LIMIT {
            let end = diagnostic.key.floor_char_boundary(SHORT_KEY_BYTES);
            diagnostic.key.truncate(end);
            diagnostic.key.push_str("...");
            diagnostic.fix = format!(
                "Field path shortened because this diagnostic exceeded {RECORD_BYTES_LIMIT} bytes; remove or correct this field using the version-1 schema."
            );
            bytes = diagnostic_bytes(&diagnostic);
        }
        debug_assert!(bytes <= RECORD_BYTES_LIMIT);
        let retained = Retained {
            diagnostic,
            bytes,
            priority,
            reason,
            group: group.0,
        };
        let stored = if entry.is_some() {
            retain(
                &mut self.entries,
                &mut self.entry_bytes,
                retained,
                RetentionLimit {
                    records: ENTRY_DIAGNOSTIC_LIMIT,
                    bytes: ENTRY_BYTES_LIMIT,
                },
            )
        } else {
            retain(
                &mut self.root,
                &mut self.root_bytes,
                retained,
                RetentionLimit {
                    records: ROOT_DIAGNOSTIC_LIMIT,
                    bytes: ROOT_BYTES_LIMIT,
                },
            )
        };
        if stored {
            if let Some(index) = entry {
                self.entry_reports.insert(index, EntryReport::Detailed);
            } else {
                self.root_seen.clear();
                self.root_unknown_seen.clear();
                for record in &self.root {
                    if let Some(root) = RootKey::parse(&record.diagnostic.key) {
                        self.root_seen.insert((root, record.reason));
                    } else if record.reason == Reason::Unknown {
                        self.root_unknown_seen.insert(record.group);
                    }
                }
            }
            if self.groups.len() < GROUP_LIMIT || self.groups.contains_key(&group) {
                *self.groups.entry(group).or_default() += 1;
            }
        }
    }

    fn finish(self) -> Diagnostics {
        let omitted = self.total - self.root.len() - self.entries.len();
        let summary = if omitted != 0 {
            let mut omitted_reasons = self.reasons;
            for record in self.root.iter().chain(&self.entries) {
                if let Some(count) = omitted_reasons.get_mut(record.reason.label()) {
                    *count -= 1;
                }
            }
            let summary = Diagnostic::new(
                DiagnosticSource::Initializer,
                "$",
                format!(
                    "Omitted {omitted} additional diagnostics (limits: {ARRAY_DIAGNOSTIC_LIMIT} details per normalized path/reason, {DIAGNOSTIC_LIMIT} records, {DIAGNOSTIC_BYTES_LIMIT} rendered bytes). Buckets without details: {}; first-error reasons: {}; omitted reasons: {}; root reasons: {}. Fix the reported fields and retry.",
                    entry_ranges(&self.entry_reports),
                    reason_counts(&self.first_reasons),
                    reason_counts(&omitted_reasons),
                    reason_counts(&self.root_reasons)
                ),
            );
            debug_assert!(diagnostic_bytes(&summary) <= MAX_SUMMARY_RECORD_BYTES);
            debug_assert!(
                diagnostic_bytes(&summary) + SUMMARY_ENVELOPE_BYTES <= SUMMARY_BYTES_RESERVE
            );
            Some(summary)
        } else {
            None
        };
        let mut errors: Vec<_> = self
            .root
            .into_iter()
            .chain(self.entries)
            .map(|record| record.diagnostic)
            .collect();
        if let Some(summary) = summary {
            errors.push(summary);
        }
        Diagnostics {
            warnings: Vec::new(),
            errors,
        }
    }
}

fn retain(
    records: &mut Vec<Retained>,
    bytes: &mut usize,
    candidate: Retained,
    limit: RetentionLimit,
) -> bool {
    if candidate.bytes > limit.bytes {
        return false;
    }
    let mut removed = Vec::new();
    let mut freed = 0;
    if candidate.priority != Priority::Extra {
        let mut eligible: Vec<_> = records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.priority < candidate.priority)
            .map(|(index, record)| (record.priority, index, record.bytes))
            .collect();
        eligible.sort_by_key(|(priority, index, _)| (*priority, std::cmp::Reverse(*index)));
        for (_, index, record_bytes) in eligible {
            if records.len() - removed.len() < limit.records
                && *bytes - freed + candidate.bytes <= limit.bytes
            {
                break;
            }
            removed.push(index);
            freed += record_bytes;
        }
    }
    if records.len() - removed.len() < limit.records
        && *bytes - freed + candidate.bytes <= limit.bytes
    {
        removed.sort_unstable_by(|left, right| right.cmp(left));
        for index in removed {
            *bytes -= records.remove(index).bytes;
        }
        *bytes += candidate.bytes;
        records.push(candidate);
        true
    } else {
        false
    }
}

fn diagnostic_bytes(record: &Diagnostic) -> usize {
    serde_json::to_vec(record)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn group_digest(key: &str) -> [u8; 32] {
    let mut normalized = String::new();
    let mut chars = key.char_indices().peekable();
    let mut quoted = false;
    let mut escaped = false;
    while let Some((offset, character)) = chars.next() {
        if quoted {
            normalized.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
            continue;
        }
        if character == '"' {
            quoted = true;
            normalized.push(character);
            continue;
        }
        if character == '['
            && chars.peek().is_some_and(|(_, c)| c.is_ascii_digit())
            && let Some(tail) = key.get(offset + 1..)
        {
            let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
            if tail.as_bytes().get(digits) == Some(&b']') {
                let end = offset + digits + 2;
                while chars.peek().is_some_and(|(index, _)| *index < end) {
                    chars.next();
                }
                if offset == "buckets".len() && key.starts_with("buckets[") {
                    if let Some(path) = key.get(offset..end) {
                        normalized.push_str(path);
                    }
                } else if end != key.len() {
                    normalized.push_str("[*]");
                }
                continue;
            }
        }
        normalized.push(character);
    }
    let mut bytes = [0; 32];
    bytes.copy_from_slice(
        ring::digest::digest(&ring::digest::SHA256, normalized.as_bytes()).as_ref(),
    );
    bytes
}

fn reason_counts(counts: &BTreeMap<&str, usize>) -> String {
    debug_assert!(counts.len() <= REASON_COUNT);
    debug_assert!(
        counts
            .keys()
            .all(|reason| reason.len() <= REASON_LABEL_BYTES)
    );
    let counts: Vec<_> = counts
        .iter()
        .filter(|(_, count)| **count != 0)
        .map(|(reason, count)| format!("{reason}={count}"))
        .collect();
    if counts.is_empty() {
        "none".into()
    } else {
        counts.join(", ")
    }
}

/// Lists the failing entries without details, up to `RANGE_BYTES_LIMIT` and then only as a count.
fn entry_ranges(reports: &BTreeMap<usize, EntryReport>) -> String {
    let mut text = String::new();
    let mut more = 0usize;
    let mut counted = reports
        .iter()
        .filter(|(_, report)| **report == EntryReport::Counted)
        .map(|(index, _)| *index)
        .peekable();
    while let Some(first) = counted.next() {
        let mut last = first;
        while let Some(next) = counted.next_if(|next| *next == last + 1) {
            last = next;
        }
        if more != 0 {
            more += 1;
            continue;
        }
        let range = if first == last {
            format!("buckets[{first}]")
        } else {
            format!("buckets[{first}..{last}]")
        };
        let separator = if text.is_empty() { "" } else { ", " };
        if text.len() + separator.len() + range.len() > RANGE_BYTES_LIMIT {
            more = 1;
            continue;
        }
        text.push_str(separator);
        text.push_str(&range);
    }
    match (text.is_empty(), more) {
        (true, _) => "none".into(),
        (false, 0) => text,
        (false, more) => format!("{text} and {more} more ranges"),
    }
}

#[derive(Debug)]
struct ResourceSizeError;

impl std::fmt::Display for ResourceSizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Encoded native resource exceeds 16 MiB; reduce it to at most 16,777,216 bytes after removing project.")
    }
}

impl std::error::Error for ResourceSizeError {}

fn check_resource_size(bytes: &[u8]) -> Result<(), ResourceSizeError> {
    if bytes.len() > ENTRY_LIMIT {
        Err(ResourceSizeError)
    } else {
        Ok(())
    }
}

// Conversion follows collecting native validation, preserving API semantics for null/defaults.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(rename = "version")]
    _version: u64,
    #[serde(rename = "project")]
    _project: Option<String>,
    #[serde(rename = "buckets")]
    _buckets: Vec<Entry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Entry {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cors: Option<Vec<Cors>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    versioning: Option<Versioning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_event_based_hold: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iam_configuration: Option<IamConfiguration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    soft_delete_policy: Option<SoftDeletePolicy>,
    /// Not a bucket resource member, so the encoded resource that the insert parser reads leaves
    /// it out; the catalog stores it as the bucket's policy instead.
    #[serde(skip_serializing)]
    iam_policy: Option<policy::IamPolicy>,
}

impl Entry {
    /// The members this entry sets. A null member sets nothing, and neither does an empty
    /// location, which selects the default one, nor a nested object without its setting.
    fn written(&self) -> Written {
        [
            (
                Member::Location,
                self.location
                    .as_deref()
                    .is_some_and(|location| !location.is_empty()),
            ),
            (Member::StorageClass, self.storage_class.is_some()),
            (Member::Labels, self.labels.is_some()),
            (Member::Cors, self.cors.is_some()),
            (
                Member::Versioning,
                self.versioning
                    .as_ref()
                    .is_some_and(|versioning| versioning.enabled.is_some()),
            ),
            (
                Member::DefaultEventBasedHold,
                self.default_event_based_hold.is_some(),
            ),
            (
                Member::PublicAccessPrevention,
                self.iam_configuration
                    .as_ref()
                    .is_some_and(|iam| iam.public_access_prevention.is_some()),
            ),
            (
                Member::SoftDeletePolicy,
                self.soft_delete_policy
                    .as_ref()
                    .is_some_and(|policy| policy.retention_duration_seconds.is_some()),
            ),
            (Member::IamPolicy, self.iam_policy.is_some()),
        ]
        .into_iter()
        .filter_map(|(member, written)| written.then_some(member))
        .collect()
    }
}

/// Only the writable field: `effectiveTime` is output-only and has no meaning in a definition.
/// The value stays untyped so the JSON API parser applies the same validation as an insert.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SoftDeletePolicy {
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_duration_seconds: Option<Value>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Versioning {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct IamConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    public_access_prevention: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Cors {
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_header: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_age_seconds: Option<i32>,
}

// Serde's derived deserializer supplies the same field names for collection and conversion.
fn schema_fields<T: DeserializeOwned>() -> &'static [&'static str] {
    struct Names<'a>(&'a mut &'static [&'static str]);
    impl<'de> de::Deserializer<'de> for Names<'_> {
        type Error = de::value::Error;
        fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
            Err(de::Error::custom("field names only"))
        }
        fn deserialize_struct<V: Visitor<'de>>(
            self,
            _: &'static str,
            fields: &'static [&'static str],
            _: V,
        ) -> Result<V::Value, Self::Error> {
            *self.0 = fields;
            Err(de::Error::custom("field names only"))
        }
        serde::forward_to_deserialize_any! { bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 char str string bytes byte_buf option unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any }
    }
    let mut names = &[][..];
    let _ = T::deserialize(Names(&mut names));
    names
}

// Raw values retain repeated members and the exact entry span, including whitespace/escapes.
struct Members<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Members<'de> {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        crate::config_input::ordered_members(deserializer, "an object", |_, _| true).map(Members)
    }
}

fn error(
    diagnostics: &mut Collected,
    reason: Reason,
    key: impl Into<String>,
    fix: impl Into<String>,
) {
    diagnostics.record(reason, key.into(), fix);
}

fn path(parent: &str, key: &str) -> String {
    if !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        if parent.is_empty() {
            key.into()
        } else {
            format!("{parent}.{key}")
        }
    } else {
        format!("{parent}[{}]", json!(key))
    }
}

fn members(raw: &RawValue) -> Option<Members<'_>> {
    serde_json::from_str(raw.get()).ok()
}

fn array(raw: &RawValue) -> Option<Vec<&RawValue>> {
    serde_json::from_str(raw.get()).ok()
}

fn value(raw: &RawValue) -> Result<Value, Diagnostics> {
    // Syntax and recursion depth were checked before inspecting any raw subtree.
    serde_json::from_str(raw.get()).map_err(|_| Diagnostics::error(
        DiagnosticSource::Initializer, "$", "Cannot decode JSON resource; use a bounded UTF-8 JSON document with native bucket settings."))
}

fn duplicates(
    raw: &RawValue,
    key: &str,
    bucket_limit: Option<NonZeroUsize>,
    diagnostics: &mut Collected,
) {
    if raw.get().starts_with('{') {
        if let Some(Members(fields)) = members(raw) {
            let mut seen = BTreeMap::new();
            for (name, raw) in fields {
                let field = path(key, &name);
                let occurrence = seen.entry(name).or_insert(0usize);
                *occurrence += 1;
                if *occurrence > 1 {
                    error(
                        diagnostics,
                        Reason::Repeated,
                        &field,
                        format!(
                            "Repeated JSON member (occurrence {occurrence}); keep one member at this path."
                        ),
                    );
                }
                duplicates(raw, &field, bucket_limit, diagnostics);
            }
        }
    } else if raw.get().starts_with('[')
        && let Some(items) = array(raw)
    {
        let inspected = if key == "buckets" {
            inspected_entries(bucket_limit, items.len())
        } else {
            items.len()
        };
        for (index, raw) in items.iter().take(inspected).enumerate() {
            duplicates(raw, &format!("{key}[{index}]"), bucket_limit, diagnostics);
        }
    }
}

fn distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let Some((first, rest)) = row.split_first_mut() else {
            return 0;
        };
        let mut diagonal = *first;
        *first = i + 1;
        let mut left = *first;
        for (cell, b) in rest.iter_mut().zip(&right) {
            let previous = *cell;
            *cell = (diagonal + usize::from(a != *b))
                .min(left + 1)
                .min(previous + 1);
            left = *cell;
            diagonal = previous;
        }
    }
    row.last().copied().unwrap_or(0)
}

fn unsupported(name: &str) -> bool {
    matches!(
        name,
        "iam"
            | "bindings"
            | "acl"
            | "defaultObjectAcl"
            | "lifecycle"
            | "uniformBucketLevelAccess"
            | "bucketPolicyOnly"
            | "autoclass"
            | "billing"
            | "customPlacementConfig"
            | "encryption"
            | "hierarchicalNamespace"
            | "ipFilter"
            | "logging"
            | "objectRetention"
            | "retentionPolicy"
            | "rpo"
            | "satisfiesPZI"
            | "satisfiesPZS"
            | "website"
            | "kind"
            | "selfLink"
            | "id"
            | "projectNumber"
            | "generation"
            | "metageneration"
            | "etag"
            | "timeCreated"
            | "updated"
            | "owner"
            | "locationType"
            | "softDeleteTime"
            | "hardDeleteTime"
    )
}

/// Advice for a member that version 1 recognizes but does not accept.
fn resource_unsupported(name: &str) -> Option<&'static str> {
    if matches!(name, "iam" | "bindings") {
        Some(
            "Unsupported in bucket initialization version 1; write bucket IAM bindings as \"iamPolicy\":{\"bindings\":[{\"role\":\"roles/storage.objectViewer\",\"members\":[\"allUsers\"]}]}.",
        )
    } else {
        unsupported(name).then_some(
            "Unsupported in bucket initialization version 1; remove this field, including empty or disabled settings.",
        )
    }
}

fn closed<T: DeserializeOwned>(fields: &Members<'_>, parent: &str, diagnostics: &mut Collected) {
    closed_with::<T>(fields, parent, diagnostics, resource_unsupported);
}

/// Reports each member `T` does not declare; `unsupported` supplies the advice for members that
/// are recognized but refused, and every other member is unknown.
fn closed_with<T: DeserializeOwned>(
    fields: &Members<'_>,
    parent: &str,
    diagnostics: &mut Collected,
    unsupported: fn(&str) -> Option<&'static str>,
) {
    let names = schema_fields::<T>();
    for (name, _) in &fields.0 {
        // Derived closed structs are the schema authority; the names only supply advice.
        if names.contains(&name.as_str()) {
            continue;
        }
        let (reason, fix) = if let Some(fix) = unsupported(name) {
            (Reason::Unsupported, fix.to_owned())
        } else if let Some(nearest) = names
            .iter()
            .min_by_key(|candidate| (distance(name, candidate), **candidate))
        {
            (
                Reason::Unknown,
                format!(
                    "Unknown field; use {nearest}, for example {}.",
                    example(nearest)
                ),
            )
        } else {
            (Reason::Unknown, "Unknown field; remove it.".into())
        };
        error(diagnostics, reason, path(parent, name), fix);
    }
}

fn example(field: &str) -> &'static str {
    match field {
        "version" => "\"version\":1",
        "buckets" => "\"buckets\":[]",
        "project" => "\"project\":\"local-project\"",
        "name" => "\"name\":\"example-bucket\"",
        "location" => "\"location\":\"US\" or \"location\":\"ASIA-NORTHEAST1\"",
        "storageClass" => "\"storageClass\":\"STANDARD\"",
        "labels" => "\"labels\":{\"env\":\"dev\"}",
        "cors" => "\"cors\":[{\"origin\":[\"https://example.invalid\"],\"method\":[\"GET\"]}]",
        "origin" => "\"origin\":[\"https://example.invalid\"]",
        "method" => "\"method\":[\"GET\"]",
        "responseHeader" => "\"responseHeader\":[\"Content-Type\"]",
        "maxAgeSeconds" => "\"maxAgeSeconds\":3600",
        "versioning" => "\"versioning\":{\"enabled\":true}",
        "enabled" => "\"enabled\":true",
        "defaultEventBasedHold" => "\"defaultEventBasedHold\":false",
        "iamConfiguration" => "\"iamConfiguration\":{\"publicAccessPrevention\":\"inherited\"}",
        "softDeletePolicy" | "retentionDurationSeconds" => {
            "\"softDeletePolicy\":{\"retentionDurationSeconds\":\"0\"} (0 or 1 to 3155760000)"
        }
        "iamPolicy" => policy::EXAMPLE,
        "bindings" => {
            "\"bindings\":[{\"role\":\"roles/storage.objectViewer\",\"members\":[\"allUsers\"]}]"
        }
        "role" => "\"role\":\"roles/storage.objectViewer\"",
        "members" => "\"members\":[\"allUsers\"]",
        _ => "\"publicAccessPrevention\":\"inherited\"",
    }
}

fn project(
    raw: &RawValue,
    key: &str,
    diagnostics: &mut Collected,
) -> Result<Option<String>, Diagnostics> {
    Ok(match value(raw)?.as_str() {
        Some(project) if validate_project(project).is_ok() => Some(project.into()),
        _ => {
            error(
                diagnostics,
                Reason::Project,
                key,
                "Project must be a nonempty identifier of at most 256 bytes without whitespace or controls; use \"project\":\"local-project\".",
            );
            None
        }
    })
}

fn native(field: &str, value: &Value, key: &str, now: SystemTime, diagnostics: &mut Collected) {
    let mut resource = serde_json::Map::from_iter([("name".into(), json!("example-bucket"))]);
    resource.insert(field.into(), value.clone());
    if parse_insert_with_profile(
        Value::Object(resource).to_string().as_bytes(),
        "local-project",
        now,
        CompatProfile::Native,
        "US",
    )
    .is_err()
    {
        diagnostics.record(
            Reason::Native,
            key.into(),
            format!("Invalid native bucket setting; use {}.", example(field)),
        );
    }
}

/// Checks a bucket entry's field names and project without decoding setting values.
fn check_shape(
    fields: &Members<'_>,
    parent: &str,
    diagnostics: &mut Collected,
) -> Result<(), Diagnostics> {
    closed::<Entry>(fields, parent, diagnostics);
    if !fields.0.iter().any(|(name, _)| name == "name") {
        error(
            diagnostics,
            Reason::Shape,
            path(parent, "name"),
            "Missing bucket name; use \"name\":\"example-bucket\".",
        );
    }
    for (name, raw) in &fields.0 {
        let key = path(parent, name);
        match name.as_str() {
            "versioning" => {
                if let Some(nested) = members(raw) {
                    closed::<Versioning>(&nested, &key, diagnostics);
                }
            }
            "iamConfiguration" => {
                if let Some(nested) = members(raw) {
                    closed::<IamConfiguration>(&nested, &key, diagnostics);
                }
            }
            "cors" => {
                if let Some(rules) = array(raw) {
                    for (index, raw) in rules.iter().enumerate() {
                        if let Some(rule) = members(raw) {
                            closed::<Cors>(&rule, &format!("{key}[{index}]"), diagnostics);
                        }
                    }
                }
            }
            "softDeletePolicy" => {
                if let Some(nested) = members(raw) {
                    closed::<SoftDeletePolicy>(&nested, &key, diagnostics);
                }
            }
            "iamPolicy" => policy::check_shape(raw, &key, diagnostics),
            "project" => {
                project(raw, &key, diagnostics)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Checks each setting value against native bucket semantics, reporting the narrowest key.
fn check_values(
    fields: &Members<'_>,
    parent: &str,
    now: SystemTime,
    diagnostics: &mut Collected,
) -> Result<(), Diagnostics> {
    for (name, raw) in &fields.0 {
        let key = path(parent, name);
        let parsed = value(raw)?;
        match name.as_str() {
            "project" => {}
            "versioning" | "iamConfiguration" => {
                if let Some(nested) = members(raw) {
                    for (child, raw) in &nested.0 {
                        if (name == "versioning" && child == "enabled")
                            || (name == "iamConfiguration" && child == "publicAccessPrevention")
                        {
                            native(
                                name,
                                &json!({child: value(raw)?}),
                                &path(&key, child),
                                now,
                                diagnostics,
                            );
                        }
                    }
                } else {
                    native(name, &parsed, &key, now, diagnostics);
                }
            }
            "softDeletePolicy" => {
                if let Some(nested) = members(raw) {
                    for (child, raw) in &nested.0 {
                        if child == "retentionDurationSeconds" {
                            native(
                                name,
                                &json!({child: value(raw)?}),
                                &path(&key, child),
                                now,
                                diagnostics,
                            );
                        }
                    }
                } else {
                    native(name, &parsed, &key, now, diagnostics);
                }
            }
            "labels" => {
                if let Some(labels) = members(raw) {
                    if labels.0.len() > 64 {
                        error(
                            diagnostics,
                            Reason::Native,
                            &key,
                            "A bucket can have at most 64 labels; reduce the label map, for example \"labels\":{\"env\":\"dev\"}.",
                        );
                    }
                    for (label, raw) in labels.0 {
                        native(
                            name,
                            &json!({&label: value(raw)?}),
                            &path(&key, &label),
                            now,
                            diagnostics,
                        );
                    }
                } else {
                    native(name, &parsed, &key, now, diagnostics);
                }
            }
            "cors" => {
                let before = diagnostics.count();
                if let Some(rules) = array(raw) {
                    for (index, raw) in rules.iter().enumerate() {
                        check_cors_rule(raw, &format!("{key}[{index}]"), now, diagnostics)?;
                    }
                    if before == diagnostics.count() {
                        native(name, &parsed, &key, now, diagnostics);
                    }
                } else {
                    native(name, &parsed, &key, now, diagnostics);
                }
            }
            "name" | "location" | "storageClass" | "defaultEventBasedHold" => {
                native(name, &parsed, &key, now, diagnostics)
            }
            "iamPolicy" => policy::check_values(raw, &key, diagnostics)?,
            _ => {}
        }
    }
    Ok(())
}

/// Checks one CORS rule field by field, and each array item separately.
fn check_cors_rule(
    raw: &RawValue,
    key: &str,
    now: SystemTime,
    diagnostics: &mut Collected,
) -> Result<(), Diagnostics> {
    let name = "cors";
    let Some(rule) = members(raw) else {
        native(name, &json!([value(raw)?]), key, now, diagnostics);
        return Ok(());
    };
    for (child, raw) in rule.0 {
        if !schema_fields::<Cors>().contains(&child.as_str()) {
            continue;
        }
        let values = value(raw)?;
        if let Some(items) = values.as_array().filter(|_| child != "maxAgeSeconds") {
            for (index, item) in items.iter().enumerate() {
                native(
                    name,
                    &json!([{&child: [item]}]),
                    &format!("{}[{index}]", path(key, &child)),
                    now,
                    diagnostics,
                );
            }
            // Empty arrays and scalar-only fields still need type checks.
            if items.is_empty() {
                native(
                    name,
                    &json!([{&child: values}]),
                    &path(key, &child),
                    now,
                    diagnostics,
                );
            }
        } else {
            native(
                name,
                &json!([{&child: values}]),
                &path(key, &child),
                now,
                diagnostics,
            );
        }
    }
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum InitOutcome {
    Disabled,
    Applied {
        created: usize,
        existing: usize,
        project_mismatches: usize,
        /// Existing buckets that still differ in a member their entry writes: in create mode one
        /// whose entry changed since it was recorded, in reconcile mode one whose location differs.
        unapplied: usize,
        /// Existing buckets that reconcile mode changed.
        reconciled: usize,
    },
}

/// One validated entry: the bucket an insert would create, the policy it starts with, and the
/// members the entry writes.
#[derive(Debug)]
pub(crate) struct InitEntry {
    pub(crate) record: BucketRecord,
    pub(crate) policy: Option<Policy>,
    pub(crate) written: Written,
}

/// Validate once, then publish missing buckets through local administrative authority.
pub async fn apply(
    catalog: &Catalog,
    config: &crate::config::Config,
    clock: std::sync::Arc<dyn Clock>,
) -> std::io::Result<InitOutcome> {
    let Some(path) = config.buckets_file.clone() else {
        return Ok(InitOutcome::Disabled);
    };
    let project = config.seed_project.clone();
    let default_location = config.default_location.clone();
    let bucket_limit = config.max_buckets_per_batch;
    let mode = config.buckets_file_mode;
    let entries = crate::task::run_blocking("bucket-initializer", move || {
        load(
            &path,
            &project,
            clock.as_ref(),
            &default_location,
            bucket_limit,
        )
    })
    .await
    .map_err(|_| {
        std::io::Error::other(Diagnostics::error(
            DiagnosticSource::Initializer,
            "buckets_file",
            "Cannot finish reading bucket initialization; inspect local resources and restart.",
        ))
    })?
    .map_err(std::io::Error::other)?;
    let (targets, prepared) =
        plan::prepare(entries).map_err(|error| initialization_failure(&error))?;
    let report = catalog
        .admin(Origin::BucketInit)
        .initialize_buckets(targets, move |observed| {
            plan::plan(prepared, observed, bucket_limit, mode)
        })
        .await
        .map_err(|error| match error {
            plan::PlanError::Admin(error) => initialization_failure(&error),
            plan::PlanError::Rejected(diagnostics) => std::io::Error::other(diagnostics),
        })?;
    report.warnings.write(std::io::stderr().lock())?;
    Ok(report.outcome())
}

fn initialization_failure(error: &AdminError) -> std::io::Error {
    let reason = match error {
        // Each store names its own remedy: a disk has no RAM budget, and RAM no data directory.
        AdminError::DiskFull(cause) => match cause.memory_budget() {
            None => "disk full; free space in the data directory",
            Some(MemoryBudget::Catalog) => {
                "memory catalog full; restart with a larger memory_catalog_max_bytes"
            }
            Some(MemoryBudget::Blobs) => {
                "memory object budget full; restart with a larger memory_max_bytes"
            }
        },
        AdminError::Busy { .. } => "administrative admission timed out",
        AdminError::Closed => "catalog closed",
        AdminError::Corrupt(_) => "catalog corrupt",
        AdminError::Io(_) => "storage I/O failure",
        AdminError::Invalid(_) => "invalid bucket definition",
        AdminError::Precondition
        | AdminError::Hold
        | AdminError::SeedProjectMismatch
        | AdminError::SourceChanged { .. }
        | AdminError::NotFound
        | AdminError::Checksum
        | AdminError::TooLarge
        | AdminError::Forbidden
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => "catalog operation failed",
    };
    std::io::Error::other(Diagnostics::error(
        DiagnosticSource::Initializer,
        "buckets_file",
        format!(
            "Cannot initialize buckets ({reason}); inspect the native catalog and available disk space, then restart to retry create-missing."
        ),
    ))
}

fn file_limit(bucket_limit: Option<NonZeroUsize>) -> usize {
    bucket_limit.map_or(UNLIMITED_FILE_LIMIT, |limit| {
        limit
            .get()
            .saturating_mul(FILE_BYTES_PER_BUCKET)
            .max(FILE_LIMIT)
    })
}

fn file_limit_fix(bucket_limit: Option<NonZeroUsize>) -> String {
    let limit = file_limit(bucket_limit);
    match bucket_limit {
        Some(_) => format!(
            "Bucket initialization file exceeds {limit} bytes; reduce it, or raise max_buckets_per_batch, which allows 1 KiB per bucket and at least 32 MiB in all."
        ),
        None => format!(
            "Bucket initialization file exceeds {limit} bytes; check that buckets_file names the intended file, or reduce it."
        ),
    }
}

/// Read one selected regular file; relative paths use the process working directory.
pub(crate) fn load(
    file: &Path,
    seed_project: &str,
    clock: &dyn Clock,
    default_location: &str,
    bucket_limit: Option<NonZeroUsize>,
) -> Result<Vec<InitEntry>, Diagnostics> {
    let read_failure = || {
        Diagnostics::error(
            DiagnosticSource::Initializer,
            "buckets_file",
            "Select a readable regular UTF-8 JSON file, for example buckets.json; symlinks to regular files are allowed.",
        )
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Inspect the opened descriptor without waiting for a FIFO writer or a path recheck.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(file).map_err(|_| read_failure())?;
    if !file.metadata().map_err(|_| read_failure())?.is_file() {
        return Err(read_failure());
    }
    let bytes = bounded_file::read_from(file, file_limit(bucket_limit) as u64).map_err(|error| Diagnostics::error(
        DiagnosticSource::Initializer, "buckets_file", match error {
            ReadError::Limit => file_limit_fix(bucket_limit),
            ReadError::Io(_) => "Cannot read bucket initialization file; select a readable local JSON file, for example buckets.json.".into(),
        }))?;
    parse(&bytes, seed_project, clock, default_location, bucket_limit)
}

/// Validate the entire document before returning any records, always using native semantics.
pub(crate) fn parse(
    bytes: &[u8],
    seed_project: &str,
    clock: &dyn Clock,
    default_location: &str,
    bucket_limit: Option<NonZeroUsize>,
) -> Result<Vec<InitEntry>, Diagnostics> {
    let failure = |key, fix| Diagnostics::error(DiagnosticSource::Initializer, key, fix);
    if bytes.len() > file_limit(bucket_limit) {
        return Err(Diagnostics::error(
            DiagnosticSource::Initializer,
            "buckets_file",
            file_limit_fix(bucket_limit),
        ));
    }
    // Value enforces UTF-8, finite numbers and serde's recursion bound before raw traversal.
    let _: Value = serde_json::from_slice(bytes).map_err(|_| failure("buckets_file", "Malformed bucket initialization file; use one UTF-8 JSON object without BOM, comments or trailing data, for example {\"version\":1,\"buckets\":[]}."))?;
    let raw: &RawValue = serde_json::from_slice(bytes).map_err(|_| {
        failure(
            "buckets_file",
            "Provide one valid JSON document, for example {\"version\":1,\"buckets\":[]}.",
        )
    })?;
    let Some(root) = members(raw) else {
        return Err(failure(
            "$",
            "Expected an object; use {\"version\":1,\"buckets\":[]}.",
        ));
    };
    let mut diagnostics = Collected::new(bucket_limit);
    duplicates(raw, "", bucket_limit, &mut diagnostics);
    closed::<Document>(&root, "", &mut diagnostics);
    let find = |name: &str| {
        root.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, raw)| *raw)
    };
    let version_valid = find("version").is_some_and(|raw| raw.get() == "1");
    for (_, raw) in root.0.iter().filter(|(key, _)| key == "version") {
        if raw.get() != "1" {
            error(
                &mut diagnostics,
                Reason::Shape,
                "version",
                "Unsupported version; use the integer token \"version\":1.",
            );
        }
    }
    if find("version").is_none() {
        error(
            &mut diagnostics,
            Reason::Shape,
            "version",
            "Missing version; add \"version\":1.",
        );
    }
    for (_, raw) in root.0.iter().filter(|(key, _)| key == "project") {
        project(raw, "project", &mut diagnostics)?;
    }
    if validate_project(seed_project).is_err() {
        error(
            &mut diagnostics,
            Reason::Project,
            "seed_project",
            "Invalid inherited project; set seed_project to local-project.",
        );
    }
    if find("buckets").is_none() {
        error(
            &mut diagnostics,
            Reason::Shape,
            "buckets",
            "Missing buckets array; use \"buckets\":[].",
        );
    }
    let now = clock.now();
    let mut names = BTreeMap::new();
    let mut checked = Vec::new();
    for (_, raw) in root.0.iter().filter(|(key, _)| key == "buckets") {
        let Some(entries) = array(raw) else {
            error(
                &mut diagnostics,
                Reason::Shape,
                "buckets",
                "Expected an array; use \"buckets\":[{\"name\":\"example-bucket\"}].",
            );
            continue;
        };
        let inspected = inspected_entries(bucket_limit, entries.len());
        if inspected < entries.len() {
            error(
                &mut diagnostics,
                Reason::Limit,
                "buckets",
                format!(
                    "Too many buckets; keep at most {inspected} entries or raise max_buckets_per_batch. Remaining {} entries were not inspected.",
                    entries.len() - inspected
                ),
            );
        }
        if !version_valid {
            continue;
        }
        for (index, raw) in entries.iter().take(inspected).enumerate() {
            if let Some(entry) = check_entry(raw, index, now, &mut names, &mut diagnostics)? {
                checked.push(entry);
            }
        }
    }
    if diagnostics.is_err() {
        return Err(diagnostics.finish());
    }
    let inherited = find("project")
        .map(value)
        .transpose()?
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| seed_project.to_owned());
    records(checked, &inherited, now, default_location, diagnostics)
}

struct CheckedEntry {
    index: usize,
    project: Option<String>,
    resource: Value,
}

/// Checks one bucket entry, recording the first index that uses each bucket name.
fn check_entry(
    raw: &RawValue,
    index: usize,
    now: SystemTime,
    names: &mut BTreeMap<String, usize>,
    diagnostics: &mut Collected,
) -> Result<Option<CheckedEntry>, Diagnostics> {
    let key = format!("buckets[{index}]");
    let raw_oversized = raw.get().len() > ENTRY_LIMIT;
    if raw_oversized {
        error(
            diagnostics,
            Reason::Limit,
            &key,
            "Entry exceeds 16 MiB; reduce its original JSON span, including project and whitespace, to at most 16,777,216 bytes.",
        );
    }
    let Some(fields) = members(raw) else {
        error(
            diagnostics,
            Reason::Shape,
            &key,
            "Expected a bucket object; use {\"name\":\"example-bucket\"}.",
        );
        return Ok(None);
    };
    let mut resource = value(raw)?;
    for (_, raw) in fields.0.iter().filter(|(field, _)| field == "name") {
        if let Some(name) = value(raw)?.as_str() {
            if let Some(first) = names.get(name) {
                if *first != index {
                    error(
                        diagnostics,
                        Reason::Duplicate,
                        path(&key, "name"),
                        format!(
                            "Duplicate bucket name; first used by buckets[{first}].name. Use a different name, for example example-bucket-other."
                        ),
                    );
                }
            } else {
                names.insert(name.to_owned(), index);
            }
        }
    }
    let project = resource
        .as_object_mut()
        .and_then(|object| object.remove("project"))
        .and_then(|value| value.as_str().map(str::to_owned));
    let encoded = resource.to_string();
    let encoded_limit = check_resource_size(encoded.as_bytes());
    let encoded_oversized = encoded_limit.is_err();
    if let Err(fix) = encoded_limit {
        error(diagnostics, Reason::Limit, &key, fix.to_string());
    }
    check_shape(&fields, &key, diagnostics)?;
    // Oversized entries are reported once; decoding their values would only add noise.
    if !raw_oversized && !encoded_oversized {
        check_values(&fields, &key, now, diagnostics)?;
    }
    Ok(Some(CheckedEntry {
        index,
        project,
        resource,
    }))
}

/// Builds the bucket records of a document whose entries all passed validation.
fn records(
    entries: Vec<CheckedEntry>,
    inherited: &str,
    now: SystemTime,
    default_location: &str,
    mut diagnostics: Collected,
) -> Result<Vec<InitEntry>, Diagnostics> {
    let failure = |key, fix| Diagnostics::error(DiagnosticSource::Initializer, key, fix);
    let mut records = Vec::with_capacity(entries.len());
    for entry in entries {
        let index = entry.index;
        let project = entry.project.unwrap_or_else(|| inherited.to_owned());
        let mut entry: Entry = serde_json::from_value(entry.resource).map_err(|_| {
            failure(
                "$",
                "Invalid initializer schema; use version 1 with native bucket resources.",
            )
        })?;
        let written = entry.written();
        let policy = entry
            .iam_policy
            .take()
            .map(|policy| policy.into_policy(&format!("buckets[{index}].iamPolicy")))
            .transpose()?;
        let encoded = serde_json::to_vec(&entry).map_err(|_| {
            failure(
                "$",
                "Cannot encode bucket resource; use supported native settings.",
            )
        })?;
        if let Err(fix) = check_resource_size(&encoded) {
            error(
                &mut diagnostics,
                Reason::Limit,
                format!("buckets[{index}]"),
                fix.to_string(),
            );
            continue;
        }
        match parse_insert_with_profile(
            &encoded,
            &project,
            now,
            CompatProfile::Native,
            default_location,
        ) {
            Ok(record) => records.push(InitEntry {
                record,
                policy,
                written,
            }),
            Err(_) => error(
                &mut diagnostics,
                Reason::Native,
                format!("buckets[{index}]"),
                "Invalid native bucket resource; use supported native settings, for example {\"name\":\"example-bucket\"}.",
            ),
        }
    }
    if diagnostics.is_err() {
        Err(diagnostics.finish())
    } else {
        Ok(records)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::diagnostics::test_support::has;
    use proptest::prelude::*;
    use std::{
        fs,
        time::{Duration, UNIX_EPOCH},
    };

    /// Labels of the form `"label123456":1e9`, which fit an entry as written and pass its limit
    /// once encoded: each takes 18 bytes as written and 27 with the number expanded.
    const EXPANDING_LABELS: usize = 800_000;

    struct FixedClock;
    impl Clock for FixedClock {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_micros(1_700_000_000_123_456)
        }
    }

    /// Most cases here check a limit an operator named, so they name one.
    const LIMIT: usize = 1000;

    fn named(limit: usize) -> Option<NonZeroUsize> {
        NonZeroUsize::new(limit)
    }

    fn records(
        entries: Result<Vec<InitEntry>, Diagnostics>,
    ) -> Result<Vec<BucketRecord>, Diagnostics> {
        entries.map(|entries| entries.into_iter().map(|entry| entry.record).collect())
    }

    fn parse(
        bytes: &[u8],
        seed_project: &str,
        clock: &dyn Clock,
        default_location: &str,
    ) -> Result<Vec<BucketRecord>, Diagnostics> {
        records(super::parse(
            bytes,
            seed_project,
            clock,
            default_location,
            named(LIMIT),
        ))
    }

    fn load(
        file: &Path,
        seed_project: &str,
        clock: &dyn Clock,
        default_location: &str,
    ) -> Result<Vec<BucketRecord>, Diagnostics> {
        records(super::load(
            file,
            seed_project,
            clock,
            default_location,
            named(LIMIT),
        ))
    }

    fn entries(document: &str) -> Result<Vec<InitEntry>, Diagnostics> {
        super::parse(
            document.as_bytes(),
            "seed-project",
            &FixedClock,
            "US",
            named(LIMIT),
        )
    }

    fn parsed_with(
        document: &str,
        limit: Option<NonZeroUsize>,
    ) -> Result<Vec<BucketRecord>, Diagnostics> {
        records(super::parse(
            document.as_bytes(),
            "seed-project",
            &FixedClock,
            "US",
            limit,
        ))
    }

    fn parsed_within(document: &str, limit: usize) -> Result<Vec<BucketRecord>, Diagnostics> {
        parsed_with(document, named(limit))
    }

    fn parsed(document: &str) -> Result<Vec<BucketRecord>, Diagnostics> {
        parsed_within(document, LIMIT)
    }

    fn rendered_bytes(errors: &Diagnostics) -> usize {
        let mut output = Vec::new();
        errors.write(&mut output).unwrap();
        output.len()
    }

    /// Every entry is valid unless `failing` says otherwise, and valid names stay unique.
    fn document_of(failing: impl Iterator<Item = bool>) -> String {
        let entries = failing
            .enumerate()
            .map(|(index, failing)| {
                if failing {
                    json!({"name":format!("bad name-{index}")})
                } else {
                    json!({"name":format!("bucket-{index:07}")})
                }
            })
            .collect::<Vec<_>>();
        json!({"version":1,"buckets":entries}).to_string()
    }

    fn keys(document: &str) -> Vec<String> {
        parsed(document)
            .unwrap_err()
            .errors
            .into_iter()
            .map(|error| error.key)
            .collect()
    }

    fn arbitrary_json() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|n| json!(n)),
            ".{0,24}".prop_map(Value::String)
        ]
        .prop_recursive(12, 256, 12, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..12).prop_map(Value::Array),
                prop::collection::btree_map(".{0,16}", inner, 0..12)
                    .prop_map(|fields| Value::Object(fields.into_iter().collect())),
            ]
        })
    }

    fn schema_values() -> BoxedStrategy<Value> {
        prop_oneof![
            arbitrary_json(),
            Just(json!(null)),
            Just(json!(1.0)),
            Just(json!("US")),
            Just(json!("STANDARD")),
            Just(json!([])),
            Just(json!({"enabled":true})),
            Just(json!({"publicAccessPrevention":"inherited"})),
            Just(json!({"env":"dev"})),
            Just(json!([{"origin":["https://example.invalid"],"method":["GET"]}])),
            Just(json!(i32::MIN)),
            Just(json!(i32::MAX)),
            Just(json!("inherited")),
            Just(json!("enforced")),
            Just(json!(["Content-Type"])),
            Just(json!(["GET"])),
            Just(json!(["https://example.invalid"]))
        ]
        .boxed()
    }

    fn schema_object<T: DeserializeOwned>() -> BoxedStrategy<Value> {
        prop::collection::btree_map(
            prop::sample::select(schema_fields::<T>().to_vec()),
            schema_values(),
            0..=schema_fields::<T>().len(),
        )
        .prop_map(|fields| {
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, value)| (key.into(), value))
                    .collect(),
            )
        })
        .boxed()
    }

    fn schema_resource() -> impl Strategy<Value = Value> {
        (
            schema_object::<Entry>(),
            prop::option::of(prop_oneof![
                schema_values(),
                prop::collection::vec(schema_object::<Cors>(), 0..4).prop_map(Value::Array)
            ]),
            prop::option::of(prop_oneof![schema_values(), schema_object::<Versioning>()]),
            prop::option::of(prop_oneof![
                schema_values(),
                schema_object::<IamConfiguration>()
            ]),
        )
            .prop_map(|(mut resource, cors, versioning, iam)| {
                let fields = resource.as_object_mut().unwrap();
                fields
                    .entry("name")
                    .or_insert_with(|| json!("example-bucket"));
                for (name, value) in [
                    ("cors", cors),
                    ("versioning", versioning),
                    ("iamConfiguration", iam),
                ] {
                    if let Some(value) = value {
                        fields.insert(name.into(), value);
                    }
                }
                resource
            })
    }

    proptest! {
        #[test]
        fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            let result = parse(&bytes, "seed-project", &FixedClock, "US");
            if let Err(errors) = result {
                prop_assert!(errors.is_err());
                let mut output = Vec::new();
                errors.write(&mut output).unwrap();
                prop_assert!(output.len() <= DIAGNOSTIC_BYTES_LIMIT);
            }
        }

        #[test]
        fn arbitrary_bytes_never_panic_at_any_limit(
            bytes in prop::collection::vec(any::<u8>(), 0..4096),
            limit in prop_oneof![Just(0usize), Just(1), 1usize..=1_000_000, Just(1_000_000)],
        ) {
            if let Err(errors) = super::parse(&bytes, "seed-project", &FixedClock, "US", named(limit)) {
                prop_assert!(errors.is_err());
                prop_assert!(rendered_bytes(&errors) <= DIAGNOSTIC_BYTES_LIMIT);
            }
        }

        #[test]
        fn arbitrary_json_shapes_and_depths_never_panic(value in arbitrary_json(), depth in 0..160usize) {
            let _ = parsed(&value.to_string());
            let nested = format!("{}{}{}", "[".repeat(depth), value, "]".repeat(depth));
            let _ = parsed(&nested);
        }

        #[test]
        fn successful_parses_preserve_input_count_names_and_order(names in prop::collection::btree_set("[a-fh-np-z1-9]{1,16}", 0..40)) {
            let expected: Vec<_> = names.into_iter().map(|name| format!("bucket-{name}")).collect();
            let document = json!({"version":1,"buckets":expected.iter().map(|name| json!({"name":name})).collect::<Vec<_>>()});
            let result = parsed(&document.to_string());
            prop_assert!(result.is_ok(), "{result:?}");
            let records = result.unwrap();
            prop_assert_eq!(records.iter().map(|record| &record.name).collect::<Vec<_>>(), expected.iter().collect::<Vec<_>>());
        }

        #[test]
        fn schema_shaped_inputs_keep_native_and_typed_conversion_consistent(resource in schema_resource()) {
            let document = json!({"version":1,"buckets":[resource]});
            match parsed(&document.to_string()) {
                Ok(records) => { prop_assert_eq!(records.len(), 1); prop_assert_eq!(records[0].name.as_str(), resource["name"].as_str().unwrap()); }
                Err(errors) => { prop_assert!(errors.errors.iter().all(|record| record.key != "$" || record.fix.starts_with("Omitted")), "{errors:?}"); }
            }
        }
    }

    proptest! {
        // Each case parses a whole document, so a few cases keep the suite fast.
        #![proptest_config(ProptestConfig::with_cases(16))]

        #[test]
        fn any_failure_pattern_at_any_limit_stays_within_the_diagnostic_bounds(
            failing in prop::collection::vec(any::<bool>(), 0..3000),
            limit in prop_oneof![Just(0usize), 1usize..3000, Just(1_000_000)],
        ) {
            let document = document_of(failing.iter().copied());
            let result = parsed_within(&document, limit);
            let within = limit == 0 || failing.len() <= limit;
            let expect_success = within && !failing.iter().any(|failing| *failing);
            prop_assert_eq!(result.is_ok(), expect_success);
            if let Err(errors) = result {
                prop_assert!(errors.errors.len() <= DIAGNOSTIC_LIMIT);
                prop_assert!(rendered_bytes(&errors) <= DIAGNOSTIC_BYTES_LIMIT);
            }
        }
    }

    #[test]
    fn diagnostics_are_bounded_with_an_exact_omitted_count() {
        let values = vec!["1"; 32_700].join(",");
        let entries = (0..15)
            .map(|index| {
                format!(r#"{{"name":"bucket-{index:04}","cors":[{{"origin":[{values}]}}]}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        let document = format!(r#"{{"version":1,"buckets":[{entries}]}}"#);
        assert!(document.len() <= FILE_LIMIT);
        let errors = parsed(&document).unwrap_err();
        assert!(errors.errors.len() <= DIAGNOSTIC_LIMIT);
        let kept = errors.errors.len() - 1;
        let omitted = 32_700 * 15 - kept;
        assert!(
            errors
                .errors
                .last()
                .unwrap()
                .fix
                .contains(&format!("Omitted {omitted} additional diagnostics"))
        );
        let mut output = Vec::new();
        errors.write(&mut output).unwrap();
        assert!(output.len() <= DIAGNOSTIC_BYTES_LIMIT);
        let retained = errors
            .errors
            .iter()
            .map(|record| {
                record.key.capacity() + record.fix.capacity() + std::mem::size_of::<Diagnostic>()
            })
            .sum::<usize>();
        assert!(
            retained
                <= 2 * DIAGNOSTIC_BYTES_LIMIT
                    + DIAGNOSTIC_LIMIT * std::mem::size_of::<Diagnostic>()
        );
        let long_key = "z".repeat(DIAGNOSTIC_BYTES_LIMIT);
        let errors =
            parsed(&json!({"version":1,"buckets":[],long_key:null}).to_string()).unwrap_err();
        assert_eq!(errors.errors.len(), 1);
        assert!(errors.errors[0].fix.contains("Field path shortened"));
        assert!(diagnostic_bytes(&errors.errors[0]) <= RECORD_BYTES_LIMIT);
    }

    #[test]
    fn diagnostic_noise_preserves_other_entries_and_document_failures() {
        let document = json!({"version":1,"extraRoot":true,"buckets":[
            {"name":"first-bucket","cors":[{"origin":vec![1;300]}]},
            {"name":"second-bucket","storageClas":"secret"},
            {"name":"third-bucket","lifecycle":{"rule":[]}}
        ]});
        let errors = parsed(&document.to_string()).unwrap_err();
        assert_eq!(
            errors
                .errors
                .iter()
                .filter(|record| record.key.starts_with("buckets[0].cors[0].origin["))
                .count(),
            ARRAY_DIAGNOSTIC_LIMIT
        );
        for key in [
            "extraRoot",
            "buckets[1].storageClas",
            "buckets[2].lifecycle",
        ] {
            assert!(
                errors.errors.iter().any(|record| record.key == key),
                "{key}"
            );
        }
        assert!(
            errors
                .errors
                .last()
                .unwrap()
                .fix
                .contains("Omitted 296 additional diagnostics")
        );
        let fields = (0..300)
            .map(|index| (format!("unknown{index}"), json!(true)))
            .chain([("name".into(), json!("first-bucket"))])
            .collect::<serde_json::Map<_, _>>();
        let document = json!({"version":1,"buckets":[fields,{"name":"second-bucket","storageClas":null},{"name":"third-bucket","lifecycle":null}]});
        let errors = parsed(&document.to_string()).unwrap_err();
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.key == "buckets[1].storageClas")
        );
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.key == "buckets[2].lifecycle")
        );
        let noise = vec!["\"noise\":1"; 300].join(",");
        let errors = parsed(&format!(
            r#"{{{noise},"version":1,"version":2,"buckets":null}}"#
        ))
        .unwrap_err();
        has(&errors, "version", "Unsupported version");
        has(&errors, "buckets", "Expected an array");
        let mut rendered = Vec::new();
        errors.write(&mut rendered).unwrap();
        assert!(rendered.len() <= DIAGNOSTIC_BYTES_LIMIT);
    }

    #[test]
    fn generalized_detail_groups_preserve_fields_after_rule_and_member_noise() {
        for rules in [vec![json!(1); 300], vec![json!({"x":1}); 300]] {
            let errors = parsed(&json!({"version":1,"buckets":[{"name":"example-bucket","cors":rules,"location":1}]}).to_string()).unwrap_err();
            assert_eq!(
                errors
                    .errors
                    .iter()
                    .filter(|record| record.key.starts_with("buckets[0].cors["))
                    .count(),
                ARRAY_DIAGNOSTIC_LIMIT
            );
            assert!(
                errors
                    .errors
                    .iter()
                    .any(|record| record.key == "buckets[0].location")
            );
        }
        let repeated = vec!["\"origin\":[]"; 300].join(",");
        let errors = parsed(&format!(r#"{{"version":1,"buckets":[{{"name":"example-bucket","cors":[{{{repeated}}}],"location":1}}]}}"#)).unwrap_err();
        assert_eq!(
            errors
                .errors
                .iter()
                .filter(|record| record.key == "buckets[0].cors[0].origin")
                .count(),
            ARRAY_DIAGNOSTIC_LIMIT
        );
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.key == "buckets[0].location")
        );
        assert_eq!(
            group_digest("buckets[0].cors[12].x"),
            group_digest("buckets[0].cors[3].x")
        );
        assert_ne!(
            group_digest("buckets[0].cors[1].x"),
            group_digest("buckets[1].cors[1].x")
        );
        assert_ne!(
            group_digest(r#"buckets[0]["x[1]"]"#),
            group_digest(r#"buckets[0]["x[2]"]"#)
        );
    }

    #[test]
    fn shortened_first_errors_and_root_typo_advice_remain_visible() {
        let long_key = "z".repeat(3000);
        let errors = parsed(
            &json!({"version":1,"buckets":[{"name":"example-bucket",long_key:1,"location":1}]})
                .to_string(),
        )
        .unwrap_err();
        assert_eq!(errors.errors.len(), 2);
        let first = errors
            .errors
            .iter()
            .find(|record| record.fix.contains("Field path shortened"))
            .unwrap();
        assert!(first.key.starts_with("buckets[0]"));
        assert!(diagnostic_bytes(first) <= RECORD_BYTES_LIMIT);
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.key == "buckets[0].location")
        );
        let noise = vec!["\"noise\":1"; 40].join(",");
        let errors = parsed(&format!(r#"{{"version":1,{noise},"bucket":[]}}"#)).unwrap_err();
        has(&errors, "bucket", "use buckets,");
        assert!(errors.errors.iter().any(|record| record.key == "buckets"));
    }

    #[test]
    fn failed_retention_never_evicts_an_existing_diagnostic() {
        let record = |key, bytes, priority| Retained {
            diagnostic: Diagnostic::new(DiagnosticSource::Initializer, key, "fix"),
            bytes,
            priority,
            reason: Reason::Shape,
            group: [0; 32],
        };
        let mut records = vec![record("old", 9, Priority::Extra)];
        let mut bytes = 9;
        assert!(!retain(
            &mut records,
            &mut bytes,
            record("too-large", 11, Priority::Structural),
            RetentionLimit {
                records: 1,
                bytes: 10
            }
        ));
        assert_eq!(records[0].diagnostic.key, "old");
        assert_eq!(bytes, 9);
        records[0].priority = Priority::Structural;
        assert!(!retain(
            &mut records,
            &mut bytes,
            record("lower-priority", 2, Priority::FirstKey),
            RetentionLimit {
                records: 1,
                bytes: 10
            }
        ));
        assert_eq!(records[0].diagnostic.key, "old");
    }

    #[test]
    fn first_error_summaries_cover_all_entries_with_bounded_ranges() {
        for alternating in [false, true] {
            let entries = (0..FULLY_LISTED_BUCKETS).map(|index| json!({"name":if !alternating || index % 2 == 0 { format!("bad name-{index}") } else { format!("bucket-{index:04}") }})).collect::<Vec<_>>();
            let errors = parsed(&json!({"version":1,"buckets":entries}).to_string()).unwrap_err();
            let summary = &errors.errors.last().unwrap().fix;
            if alternating {
                assert!(summary.contains("buckets[478]"));
                assert!(summary.contains("buckets[998]"));
                assert!(summary.contains("native-value=500"));
            } else {
                assert!(summary.contains("buckets[239..999]"));
                assert!(summary.contains("native-value=1000"));
            }
            assert!(!summary.contains("more ranges"), "{summary}");
            assert!(errors.errors.len() <= DIAGNOSTIC_LIMIT);
            assert!(rendered_bytes(&errors) <= DIAGNOSTIC_BYTES_LIMIT);
        }
    }

    /// Alternating failures make one range per failing entry: without a byte limit on the list,
    /// this summary alone would be about 400 KiB against a 64 KiB budget for the whole report.
    #[test]
    fn a_raised_limit_shortens_the_summary_and_counts_the_ranges_it_left_out() {
        const COUNT: usize = 40_000;
        let document = document_of((0..COUNT).map(|index| index % 2 == 0));
        let errors = parsed_within(&document, COUNT).unwrap_err();
        assert!(errors.errors.len() <= DIAGNOSTIC_LIMIT);
        assert!(rendered_bytes(&errors) <= DIAGNOSTIC_BYTES_LIMIT);
        let summary = &errors.errors.last().unwrap().fix;
        assert!(summary.contains("native-value=20000"), "{summary}");
        let (ranges, rest) = summary
            .split_once(" more ranges; ")
            .expect("a list past the byte limit ends with a count");
        let (ranges, more) = ranges.rsplit_once(" and ").unwrap();
        let listed = ranges.matches("buckets[").count();
        let detailed = errors.errors.len() - 1;
        assert_eq!(
            listed + more.parse::<usize>().unwrap() + detailed,
            COUNT / 2
        );
        assert!(rest.starts_with("first-error reasons:"), "{summary}");
    }

    #[test]
    fn the_bucket_limit_is_inclusive_and_its_failure_names_the_setting() {
        for limit in [1, 2, LIMIT + 1, 5000] {
            let at_limit = document_of((0..limit).map(|_| false));
            assert_eq!(parsed_within(&at_limit, limit).unwrap().len(), limit);
            let past_limit = document_of((0..=limit).map(|_| false));
            let errors = parsed_within(&past_limit, limit).unwrap_err();
            assert_eq!(errors.errors.len(), 1, "{errors}");
            assert_eq!(errors.errors[0].key, "buckets");
            assert!(
                errors.errors[0]
                    .fix
                    .contains(&format!("at most {limit} entries")),
                "{errors}"
            );
            assert!(
                errors.errors[0].fix.contains("max_buckets_per_batch"),
                "{errors}"
            );
            assert!(
                errors.errors[0]
                    .fix
                    .contains("Remaining 1 entries were not inspected"),
                "{errors}"
            );
        }
    }

    #[test]
    fn no_bucket_limit_inspects_every_entry_up_to_the_file_limit() {
        let entries = (0..LIMIT + 1)
            .map(|index| json!({"name":format!("bucket-{index:04}")}))
            .collect::<Vec<_>>();
        let document = json!({"version":1,"buckets":entries}).to_string();
        assert_eq!(parsed_with(&document, None).unwrap().len(), LIMIT + 1);
        assert_eq!(file_limit(None), UNLIMITED_FILE_LIMIT);
        let fix = file_limit_fix(None);
        assert!(fix.contains("buckets_file"), "{fix}");
        assert!(!fix.contains("max_buckets_per_batch"), "{fix}");
    }

    #[test]
    fn the_file_limit_follows_the_bucket_limit_and_never_shrinks_below_its_floor() {
        let file_limit = |limit| file_limit(named(limit));
        // The first bucket limit whose allowance passes the floor.
        let limit = FILE_LIMIT / FILE_BYTES_PER_BUCKET + 1;
        assert_eq!(file_limit(1), FILE_LIMIT);
        assert_eq!(file_limit(LIMIT), FILE_LIMIT);
        assert_eq!(file_limit(FILE_LIMIT / FILE_BYTES_PER_BUCKET), FILE_LIMIT);
        assert_eq!(file_limit(limit), limit * FILE_BYTES_PER_BUCKET);
        assert_eq!(file_limit(1_000_000), 1_000_000 * FILE_BYTES_PER_BUCKET);

        let directory = crate::test_support::TestDir::new();
        let file = directory.0.join("buckets.json");
        let empty = r#"{"version":1,"buckets":[]}"#;
        let fill = |extra: usize| {
            format!(
                "{empty}{}",
                " ".repeat(file_limit(limit) - empty.len() + extra)
            )
        };
        assert!(parsed_within(&fill(0), limit).is_ok());
        for failure in [
            parsed_within(&fill(1), limit).unwrap_err(),
            {
                fs::write(&file, fill(1)).unwrap();
                super::load(&file, "seed-project", &FixedClock, "US", named(limit)).unwrap_err()
            },
            {
                fs::write(&file, fill(0)).unwrap();
                load(&file, "seed-project", &FixedClock, "US").unwrap_err()
            },
        ] {
            assert_eq!(failure.errors[0].key, "buckets_file");
            assert!(
                failure.errors[0].fix.contains("max_buckets_per_batch"),
                "{failure}"
            );
        }
        fs::write(&file, fill(0)).unwrap();
        assert!(super::load(&file, "seed-project", &FixedClock, "US", named(limit)).is_ok());
    }

    #[test]
    fn oversized_entries_keep_structural_and_duplicate_name_diagnostics() {
        let base = r#"{"name":"first-bucket","storageClas":"secret""#;
        let oversized = format!("{base}{}}}", " ".repeat(ENTRY_LIMIT));
        let document =
            format!(r#"{{"version":1,"buckets":[{oversized},{{"name":"first-bucket"}}]}}"#);
        let errors = parsed(&document).unwrap_err();
        for key in ["buckets[0]", "buckets[0].storageClas", "buckets[1].name"] {
            assert!(
                errors.errors.iter().any(|record| record.key == key),
                "{key}"
            );
        }
        let values = (0..EXPANDING_LABELS)
            .map(|index| format!(r#""label{index}":1e9"#))
            .collect::<Vec<_>>()
            .join(",");
        let document = format!(
            r#"{{"version":1,"buckets":[{{"name":"first-bucket","storageClas":null,"labels":{{{values}}}}}]}}"#
        );
        let errors = parsed(&document).unwrap_err();
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.key == "buckets[0].storageClas")
        );
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.fix.contains("Encoded native resource"))
        );
    }

    #[test]
    fn entries_after_the_count_limit_are_not_interpreted() {
        let mut entries = (0..LIMIT)
            .map(|index| json!({"name":format!("bucket-{index:04}")}))
            .collect::<Vec<_>>();
        entries.push(json!({"unknown":true}));
        let errors = parsed(&json!({"version":1,"buckets":entries}).to_string()).unwrap_err();
        assert_eq!(errors.errors.len(), 1);
        assert_eq!(errors.errors[0].key, "buckets");
        assert!(
            errors.errors[0]
                .fix
                .contains("Remaining 1 entries were not inspected")
        );
    }

    #[test]
    fn native_settings_project_precedence_and_injected_microseconds() {
        let document = r#"{"version":1,"project":"file-project","buckets":[
            {"name":"first-bucket","project":"entry-project","location":"EU","storageClass":"NEARLINE",
             "labels":{"env":"dev"},"cors":[{"origin":["https://example.invalid"],"method":["GET"],"responseHeader":["Content-Type"],"maxAgeSeconds":3600}],
             "versioning":{"enabled":true},"defaultEventBasedHold":true,"iamConfiguration":{"publicAccessPrevention":"enforced"}},
            {"name":"second-bucket"}]}"#;
        let records = parsed(document).unwrap();
        assert_eq!(records[0].project, "entry-project");
        assert_eq!(records[1].project, "file-project");
        assert_eq!(records[0].time_created, FixedClock.now());
        assert_eq!(records[0].updated, FixedClock.now());
        assert_eq!(records[0].metageneration, 1);
        assert_eq!(records[0].location, "EU");
        assert_eq!(records[0].storage_class.as_str(), "NEARLINE");
        assert_eq!(records[0].labels["env"], "dev");
        assert_eq!(records[0].cors[0].max_age_seconds, Some(3600));
        assert!(records[0].versioning_enabled);
        assert!(records[0].default_event_based_hold);
        assert!(records[0].public_access_prevention_enforced);
        let defaults = parsed(r#"{"version":1,"buckets":[{"name":"default-bucket","cors":null,"versioning":{},"iamConfiguration":{}}]}"#).unwrap();
        assert_eq!(defaults[0].project, "seed-project");
        assert_eq!(defaults[0].location, "US");
        assert_eq!(defaults[0].storage_class.as_str(), "STANDARD");
        assert!(!defaults[0].versioning_enabled);
        assert!(!defaults[0].default_event_based_hold);
        assert!(!defaults[0].public_access_prevention_enforced);
        assert!(parsed(r#"{"version":1,"buckets":[]}"#).unwrap().is_empty());
    }

    #[test]
    fn initializer_location_normalizes_names_and_defaults() {
        let records = parse(br#"{"version":1,"buckets":[{"name":"default-bucket"},{"name":"explicit-bucket","location":"EU"}]}"#, "seed-project", &FixedClock, "ASIA").unwrap();
        assert_eq!(records[0].location, "ASIA");
        assert_eq!(records[1].location, "EU");
        for value in [json!(""), Value::Null] {
            let document =
                json!({"version":1,"buckets":[{"name":"example-bucket","location":value}]})
                    .to_string();
            assert_eq!(
                parse(document.as_bytes(), "seed-project", &FixedClock, "NAM4").unwrap()[0]
                    .location,
                "NAM4"
            );
        }
        for (value, kind) in [("asia-northeast1", "region"), ("eur4", "dual-region")] {
            let document =
                json!({"version":1,"buckets":[{"name":"example-bucket","location":value}]})
                    .to_string();
            let records = parse(document.as_bytes(), "seed-project", &FixedClock, "US").unwrap();
            assert_eq!(records[0].location, value.to_ascii_uppercase());
            assert_eq!(records[0].location_type, kind);
        }
        for value in [json!("invalid"), json!("MARS-CENTRAL1"), json!(1)] {
            let document =
                json!({"version":1,"buckets":[{"name":"example-bucket","location":value}]})
                    .to_string();
            assert!(parse(document.as_bytes(), "seed-project", &FixedClock, "ASIA").is_err());
        }
    }

    #[test]
    fn malformed_documents_versions_and_shapes_fail_without_parser_leaks() {
        for bytes in [
            b"\xff".as_slice(),
            b"\xef\xbb\xbf{}",
            b"",
            b"{} {}",
            b"{/* secret */}",
            b"{\"secret-token\":",
        ] {
            let errors = parse(bytes, "seed-project", &FixedClock, "US").unwrap_err();
            assert_eq!(errors.errors.len(), 1);
            assert_eq!(errors.errors[0].key, "buckets_file");
            assert!(!errors.to_string().contains("secret"));
        }
        for document in ["null", "[]", "true", "1", "\"text\""] {
            assert_eq!(keys(document), ["$"]);
        }
        for version in ["null", "0", "2", "1.0", "1e0", "true", "\"1\"", "[]", "{}"] {
            let document = format!(r#"{{"version":{version},"buckets":[{{"secret":1}}]}}"#);
            assert_eq!(keys(&document), ["version"], "{version}");
        }
        assert_eq!(keys("{}"), ["version", "buckets"]);
        assert_eq!(keys(r#"{"version":1,"buckets":null}"#), ["buckets"]);
        assert_eq!(
            keys(r#"{"version":1,"buckets":[null,1,"name",[]]}"#),
            ["buckets[0]", "buckets[1]", "buckets[2]", "buckets[3]"]
        );
        let deep = format!("{}0{}", "[".repeat(130), "]".repeat(130));
        assert!(parsed(&deep).is_err());
    }

    #[test]
    fn inclusive_file_entry_and_count_limits_include_original_bytes() {
        let empty = r#"{"version":1,"buckets":[]}"#;
        let at_file = format!("{empty}{}", " ".repeat(FILE_LIMIT - empty.len()));
        assert!(parsed(&at_file).is_ok());
        assert_eq!(keys(&format!("{at_file} ")), ["buckets_file"]);
        for prefix in [
            r#"{"name":"example-bucket""#,
            r#"{"name":"example-bucket","project":"entry-project""#,
            r#"{"name":"ex\u0061mple-bucket""#,
        ] {
            let entry = format!("{prefix}{}}}", " ".repeat(ENTRY_LIMIT - prefix.len() - 1));
            assert_eq!(entry.len(), ENTRY_LIMIT);
            assert!(parsed(&format!(r#"{{"version":1,"buckets":[  {entry}  ]}}"#)).is_ok());
            assert_eq!(
                keys(&format!(
                    r#"{{"version":1,"buckets":[{prefix}{} }}]}}"#,
                    " ".repeat(ENTRY_LIMIT - prefix.len() - 1)
                )),
                ["buckets[0]"]
            );
        }
        for count in [1000, 1001] {
            let entries = (0..count)
                .map(|i| format!(r#"{{"name":"bucket-{i:04}"}}"#))
                .collect::<Vec<_>>()
                .join(",");
            let result = parsed(&format!(r#"{{"version":1,"buckets":[{entries}]}}"#));
            if count == 1000 {
                assert_eq!(result.unwrap().len(), count);
            } else {
                assert_eq!(result.unwrap_err().errors[0].key, "buckets");
            }
        }
    }

    #[test]
    fn encoded_resource_cannot_expand_past_the_native_body_limit() {
        let resource = json!({"name":"abc","cors":[{"origin":[""]}]}).to_string();
        let exact = resource.replacen(
            "[\"\"]",
            &format!("[\"{}\"]", "a".repeat(ENTRY_LIMIT - resource.len())),
            1,
        );
        assert_eq!(exact.len(), ENTRY_LIMIT);
        assert!(parsed(&format!(r#"{{"version":1,"buckets":[{exact}]}}"#)).is_ok());
        let base = r#"{"name":"abc","cors":[{"origin":[""],"maxAgeSeconds":1000}]}"#;
        let encoded_len = serde_json::from_str::<Value>(base)
            .unwrap()
            .to_string()
            .len();
        for extra in [0, 1] {
            let entry = base.replacen(
                "[\"\"]",
                &format!("[\"{}\"]", "a".repeat(ENTRY_LIMIT - encoded_len + extra)),
                1,
            );
            assert_eq!(
                serde_json::from_str::<Value>(&entry)
                    .unwrap()
                    .to_string()
                    .len(),
                ENTRY_LIMIT + extra
            );
            assert!(
                parse_insert_with_profile(
                    entry.as_bytes(),
                    "local-project",
                    FixedClock.now(),
                    CompatProfile::Native,
                    "US"
                )
                .is_ok()
            );
            if extra == 0 {
                assert!(check_resource_size(entry.as_bytes()).is_ok());
                assert!(parsed(&format!(r#"{{"version":1,"buckets":[{entry}]}}"#)).is_ok());
            } else {
                assert_eq!(
                    check_resource_size(entry.as_bytes())
                        .unwrap_err()
                        .to_string(),
                    "Encoded native resource exceeds 16 MiB; reduce it to at most 16,777,216 bytes after removing project."
                );
            }
        }
        assert_eq!(
            keys(
                r#"{"version":1,"buckets":[{"name":"example-bucket","cors":[{"maxAgeSeconds":1e3}]}]}"#
            ),
            ["buckets[0].cors[0].maxAgeSeconds"]
        );
        let labels = (0..EXPANDING_LABELS)
            .map(|i| format!(r#""label{i}":1e9"#))
            .collect::<Vec<_>>()
            .join(",");
        let entry = format!(r#"{{"name":"example-bucket","labels":{{{labels}}}}}"#);
        assert!(entry.len() < ENTRY_LIMIT);
        assert!(
            serde_json::from_str::<Value>(&entry)
                .unwrap()
                .to_string()
                .len()
                > ENTRY_LIMIT
        );
        let errors = parsed(&format!(
            r#"{{"version":1,"buckets":[{entry},{{"name":"other-bucket","location":"invalid"}}]}}"#
        ))
        .unwrap_err();
        has(&errors, "buckets[0]", "Encoded");
        assert!(
            errors
                .errors
                .iter()
                .any(|error| error.key == "buckets[1].location")
        );
    }

    #[test]
    fn invalid_projects_do_not_hide_independent_native_errors() {
        let document = r#"{"version":1,"project":"secret bad","buckets":[
            {"name":"UPPERCASE","project":"entry-project","location":"secret-location","storageClass":"secret-class","labels":{"Bad":"bad","good":1},
             "cors":[{"origin":["bad origin","also bad"],"method":["bad method"],"responseHeader":["bad header"],"maxAgeSeconds":-1}],
             "versioning":{"enabled":1},"defaultEventBasedHold":null,"iamConfiguration":{"publicAccessPrevention":"secret-pap"}},
            {"name":"second-bucket","project":null,"location":"bad"}]}"#;
        let errors = parsed(document).unwrap_err();
        let paths: Vec<_> = errors
            .errors
            .iter()
            .map(|error| error.key.as_str())
            .collect();
        for expected in [
            "project",
            "buckets[0].name",
            "buckets[0].location",
            "buckets[0].storageClass",
            "buckets[0].labels.Bad",
            "buckets[0].labels.good",
            "buckets[0].cors[0].origin[0]",
            "buckets[0].cors[0].origin[1]",
            "buckets[0].cors[0].method[0]",
            "buckets[0].cors[0].responseHeader[0]",
            "buckets[0].cors[0].maxAgeSeconds",
            "buckets[0].versioning.enabled",
            "buckets[0].defaultEventBasedHold",
            "buckets[0].iamConfiguration.publicAccessPrevention",
            "buckets[1].project",
            "buckets[1].location",
        ] {
            assert!(paths.contains(&expected), "missing {expected}: {paths:?}");
        }
        assert_eq!(paths.len(), 16);
        assert!(!errors.to_string().contains("secret"));
        for project in [
            json!(null),
            json!(""),
            json!("a b"),
            json!("a\n"),
            json!("x".repeat(257)),
            json!(1),
        ] {
            let document = json!({"version":1,"project":project,"buckets":[{"name":"example-bucket","project":"valid-project"}]}).to_string();
            assert_eq!(keys(&document), ["project"]);
        }
        assert_eq!(
            parse(
                br#"{"version":1,"buckets":[{"name":"example-bucket","location":"bad"}]}"#,
                "bad project",
                &FixedClock,
                "US"
            )
            .unwrap_err()
            .errors
            .len(),
            2
        );
    }

    #[test]
    fn closed_schema_rejects_unknown_unsupported_and_disabled_noops() {
        for (field, value) in [
            ("iam", json!({})),
            ("bindings", json!([])),
            ("acl", json!([])),
            ("defaultObjectAcl", json!([])),
            ("timeCreated", json!("secret")),
            ("lifecycle", json!(null)),
            ("lifecycle", json!({})),
            ("lifecycle", json!({"rule":[]})),
            ("lifecycle", json!({"rule":[{}]})),
        ] {
            let document =
                json!({"version":1,"buckets":[{"name":"example-bucket",field:value}]}).to_string();
            let errors = parsed(&document).unwrap_err();
            assert_eq!(errors.errors.len(), 1, "{document}");
            assert!(errors.errors[0].fix.contains("Unsupported"));
            // Bindings belong in iamPolicy, so the advice for the IAM spellings points there.
            assert_eq!(
                errors.errors[0].fix.contains("\"iamPolicy\""),
                matches!(field, "iam" | "bindings"),
                "{document}"
            );
        }
        for field in ["uniformBucketLevelAccess", "bucketPolicyOnly"] {
            let document = json!({"version":1,"buckets":[{"name":"example-bucket","iamConfiguration":{field:{"enabled":false}}}]}).to_string();
            assert_eq!(
                keys(&document),
                [format!("buckets[0].iamConfiguration.{field}")]
            );
        }
        let errors = parsed(r#"{"version":1,"projec":"secret","buckets":[{"name":"example-bucket","storageClas":"secret","versioning":{"enable":true},"cors":[{"origins":[]}],"iamConfiguration":{"publicAccessPreventio":"secret"}}]}"#).unwrap_err();
        assert_eq!(errors.errors.len(), 5);
        assert!(errors.errors[1].fix.contains("storageClass"));
        assert!(!errors.to_string().contains("secret"));
        assert_eq!(distance("aa", "ab"), 1);
        let raw: &RawValue = serde_json::from_str(r#"{"":1}"#).unwrap();
        let mut errors = Collected::new(named(LIMIT));
        closed::<Document>(&members(raw).unwrap(), "", &mut errors);
        assert!(errors.finish().errors[0].fix.contains("use buckets,"));
    }

    #[test]
    fn iam_policy_keeps_the_written_bindings_and_drops_empty_ones_like_set_iam_policy() {
        let document = json!({"version":1,"buckets":[
            {"name":"public-bucket","iamPolicy":{"bindings":[
                {"role":"roles/storage.objectViewer","members":["allUsers","user:reader@example.invalid"]},
                {"role":"roles/storage.legacyBucketReader","members":[]}
            ]}},
            {"name":"cleared-bucket","iamPolicy":{"bindings":[]}},
            {"name":"default-bucket"}
        ]})
        .to_string();
        let entries = entries(&document).unwrap();
        assert_eq!(
            entries[0].policy,
            Some(Policy {
                bindings: vec![crate::auth::Binding {
                    role: "roles/storage.objectViewer".into(),
                    members: vec!["allUsers".into(), "user:reader@example.invalid".into()],
                }],
                etag: None,
            })
        );
        assert_eq!(
            entries[1].policy,
            Some(Policy {
                bindings: Vec::new(),
                etag: None
            })
        );
        assert_eq!(entries[2].policy, None);
        // The policy is not a bucket resource member, so it does not reach the record.
        assert_eq!(entries[0].record.name, "public-bucket");
    }

    #[test]
    fn entry_digests_follow_what_an_entry_sets_not_how_it_is_spelled() {
        let digest = |entry: Value, project: Option<&str>| {
            let mut document = json!({"version":1,"buckets":[entry]});
            if let Some(project) = project {
                document["project"] = json!(project);
            }
            let entry = entries(&document.to_string()).unwrap().remove(0);
            entry
                .written
                .record(&entry.record, entry.policy.as_ref())
                .digest
        };
        let base = digest(
            json!({"name":"first-bucket","location":"US","labels":{"a":"1","b":"2"},
                "softDeletePolicy":{"retentionDurationSeconds":5},
                "iamPolicy":{"bindings":[
                    {"role":"roles/storage.objectViewer","members":["allUsers","user:a@example.invalid"]},
                    {"role":"roles/storage.objectCreator","members":["user:b@example.invalid"]}]}}),
            None,
        );
        for (same, project) in [
            // The name selects the bucket and the project is compared on its own.
            (
                json!({"name":"other-bucket","location":"US","labels":{"a":"1","b":"2"},
                    "softDeletePolicy":{"retentionDurationSeconds":5},
                    "iamPolicy":{"bindings":[
                        {"role":"roles/storage.objectViewer","members":["allUsers","user:a@example.invalid"]},
                        {"role":"roles/storage.objectCreator","members":["user:b@example.invalid"]}]}}),
                Some("other-project"),
            ),
            // Case, number spelling, member order, binding order, repeats and null members.
            (
                json!({"name":"first-bucket","location":"us","labels":{"b":"2","a":"1"},
                    "softDeletePolicy":{"retentionDurationSeconds":"5"},"cors":null,
                    "iamPolicy":{"bindings":[
                        {"role":"roles/storage.objectCreator","members":["user:b@example.invalid"]},
                        {"role":"roles/storage.objectViewer","members":["user:a@example.invalid"]},
                        {"role":"roles/storage.objectViewer","members":["allUsers","allUsers"]},
                        {"role":"roles/storage.legacyBucketReader","members":[]}]}}),
                None,
            ),
        ] {
            assert_eq!(digest(same, project), base);
        }
        for changed in [
            json!({"name":"first-bucket","location":"EU","labels":{"a":"1","b":"2"},
                "softDeletePolicy":{"retentionDurationSeconds":5},
                "iamPolicy":{"bindings":[
                    {"role":"roles/storage.objectViewer","members":["allUsers","user:a@example.invalid"]},
                    {"role":"roles/storage.objectCreator","members":["user:b@example.invalid"]}]}}),
            json!({"name":"first-bucket","location":"US","labels":{"a":"1"},
                "softDeletePolicy":{"retentionDurationSeconds":5},
                "iamPolicy":{"bindings":[
                    {"role":"roles/storage.objectViewer","members":["allUsers","user:a@example.invalid"]},
                    {"role":"roles/storage.objectCreator","members":["user:b@example.invalid"]}]}}),
            json!({"name":"first-bucket","location":"US","labels":{"a":"1","b":"2"},
                "iamPolicy":{"bindings":[
                    {"role":"roles/storage.objectViewer","members":["allUsers","user:a@example.invalid"]},
                    {"role":"roles/storage.objectCreator","members":["user:b@example.invalid"]}]}}),
            json!({"name":"first-bucket","location":"US","labels":{"a":"1","b":"2"},
                "softDeletePolicy":{"retentionDurationSeconds":5},
                "iamPolicy":{"bindings":[
                    {"role":"roles/storage.objectViewer","members":["user:a@example.invalid"]},
                    {"role":"roles/storage.objectCreator","members":["user:b@example.invalid"]}]}}),
            json!({"name":"first-bucket","location":"US","labels":{"a":"1","b":"2"},
                "softDeletePolicy":{"retentionDurationSeconds":5}}),
        ] {
            assert_ne!(digest(changed, None), base);
        }
        // Writing the default value is still writing the member.
        assert_ne!(
            digest(
                json!({"name":"first-bucket","storageClass":"STANDARD"}),
                None
            ),
            digest(json!({"name":"first-bucket"}), None)
        );
    }

    #[test]
    fn iam_policy_reports_each_invalid_part_at_its_own_key_without_echoing_values() {
        let policy = |policy: Value| {
            json!({"version":1,"buckets":[{"name":"example-bucket","iamPolicy":policy}]})
                .to_string()
        };
        let binding = |binding: Value| policy(json!({"bindings":[binding]}));
        for (document, key, fix) in [
            (policy(json!(null)), "buckets[0].iamPolicy", "policy object"),
            (policy(json!([])), "buckets[0].iamPolicy", "policy object"),
            (
                policy(json!({})),
                "buckets[0].iamPolicy.bindings",
                "Missing bindings",
            ),
            (
                policy(json!({"bindings":{}})),
                "buckets[0].iamPolicy.bindings",
                "array of bindings",
            ),
            (
                policy(json!({"bindings":[],"version":3})),
                "buckets[0].iamPolicy.version",
                "Unsupported",
            ),
            (
                policy(json!({"bindings":[],"etag":"CAE="})),
                "buckets[0].iamPolicy.etag",
                "Unsupported",
            ),
            (
                policy(json!({"bindings":[],"auditConfigs":[]})),
                "buckets[0].iamPolicy.auditConfigs",
                "Unsupported",
            ),
            (
                policy(json!({"bindings":[],"bindingz":[]})),
                "buckets[0].iamPolicy.bindingz",
                "use bindings",
            ),
            (
                binding(json!("roles/storage.objectViewer")),
                "buckets[0].iamPolicy.bindings[0]",
                "binding object",
            ),
            (
                binding(
                    json!({"role":"roles/storage.objectViewer","members":["allUsers"],"condition":{}}),
                ),
                "buckets[0].iamPolicy.bindings[0].condition",
                "Conditional",
            ),
            (
                binding(json!({"members":["allUsers"]})),
                "buckets[0].iamPolicy.bindings[0].role",
                "Missing role",
            ),
            (
                binding(json!({"role":"roles/storage.objectViewer"})),
                "buckets[0].iamPolicy.bindings[0].members",
                "Missing members",
            ),
            (
                binding(json!({"role":"roles/owner","members":["allUsers"]})),
                "buckets[0].iamPolicy.bindings[0].role",
                "Basic and custom roles",
            ),
            (
                binding(json!({"role":"roles/storage.hmacKeyAdmin","members":["allUsers"]})),
                "buckets[0].iamPolicy.bindings[0].role",
                "granted on a bucket",
            ),
            (
                binding(json!({"role":"secret-role","members":["allUsers"]})),
                "buckets[0].iamPolicy.bindings[0].role",
                "granted on a bucket",
            ),
            (
                binding(json!({"role":1,"members":["allUsers"]})),
                "buckets[0].iamPolicy.bindings[0].role",
                "granted on a bucket",
            ),
            (
                binding(json!({"role":"roles/storage.objectViewer","members":"allUsers"})),
                "buckets[0].iamPolicy.bindings[0].members",
                "array of members",
            ),
            (
                binding(
                    json!({"role":"roles/storage.objectViewer","members":["allUsers","secret-member"]}),
                ),
                "buckets[0].iamPolicy.bindings[0].members[1]",
                "Invalid member",
            ),
            (
                binding(json!({"role":"roles/storage.objectViewer","members":[1]})),
                "buckets[0].iamPolicy.bindings[0].members[0]",
                "Invalid member",
            ),
            (
                binding(
                    json!({"role":"roles/storage.objectViewer","members":["group:secret@example.invalid"]}),
                ),
                "buckets[0].iamPolicy.bindings[0].members[0]",
                "Groups, domains",
            ),
            (
                binding(
                    json!({"role":"roles/storage.objectViewer","members":vec!["allUsers"; 1501]}),
                ),
                "buckets[0].iamPolicy",
                "at most 1500 principals",
            ),
        ] {
            let errors = parsed(&document).unwrap_err();
            has(&errors, key, fix);
            assert!(!errors.to_string().contains("secret"), "{errors}");
        }
        // The occurrence limit counts every binding, as setIamPolicy does.
        let split = policy(json!({"bindings":[
            {"role":"roles/storage.objectViewer","members":vec!["allUsers"; 750]},
            {"role":"roles/storage.objectCreator","members":vec!["allUsers"; 751]}
        ]}));
        has(
            &parsed(&split).unwrap_err(),
            "buckets[0].iamPolicy",
            "at most 1500 principals",
        );
        let at_limit = policy(json!({"bindings":[
            {"role":"roles/storage.objectViewer","members":vec!["allUsers"; 750]},
            {"role":"roles/storage.objectCreator","members":vec!["allUsers"; 750]}
        ]}));
        assert!(parsed(&at_limit).is_ok());
    }

    #[test]
    fn repeated_members_at_every_depth_and_decoded_global_names_are_reported() {
        let errors = parsed(r#"{"version":1,"buckets":[{"name":"first-bucket","name":"second-bucket"},{"name":"first-bucket"}]}"#).unwrap_err();
        has(&errors, "buckets[1].name", "buckets[0].name");
        let document = r#"{"version":1,"version":1,"buckets":[
            {"name":"example-bucket","project":"one","project":"one","labels":{"env":"dev","env":"test"},
             "versioning":{"enabled":true,"enabled":false},"cors":[{"origin":[],"origin":[]}],
             "iamConfiguration":{"publicAccessPrevention":"inherited","publicAccessPrevention":"enforced"}},
            {"name":"ex\u0061mple-bucket","project":"two"}]}"#;
        let errors = parsed(document).unwrap_err();
        assert_eq!(errors.errors.len(), 7);
        for expected in [
            "version",
            "buckets[0].project",
            "buckets[0].labels.env",
            "buckets[0].versioning.enabled",
            "buckets[0].cors[0].origin",
            "buckets[0].iamConfiguration.publicAccessPrevention",
            "buckets[1].name",
        ] {
            assert!(errors.errors.iter().any(|error| error.key == expected));
        }
        assert!(
            errors
                .errors
                .last()
                .unwrap()
                .fix
                .contains("buckets[0].name")
        );
        let errors = parsed(r#"{"version":1,"buckets":[{"name":"example-bucket","lifecycle":{"x":{"a":1,"a":2}}}]}"#).unwrap_err();
        assert!(
            errors
                .errors
                .iter()
                .any(|error| error.key == "buckets[0].lifecycle.x.a")
        );
    }

    #[test]
    fn initialization_failure_reports_catalog_corruption() {
        assert_eq!(
            initialization_failure(&AdminError::Corrupt("invalid stored enum".into())).to_string(),
            "buckets_file: Cannot initialize buckets (catalog corrupt); inspect the native catalog and available disk space, then restart to retry create-missing."
        );
    }

    #[test]
    fn a_full_store_names_only_the_running_backends_remedy() {
        use crate::storage::failure::{IoCause, MemoryExhausted};
        let full = |budget: Option<MemoryBudget>| {
            let error = match budget {
                None => std::io::Error::from(std::io::ErrorKind::StorageFull),
                Some(budget) => std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    MemoryExhausted::new(budget),
                ),
            };
            initialization_failure(&AdminError::DiskFull(IoCause::from(error))).to_string()
        };
        let disk = full(None);
        assert!(
            disk.contains("free space in the data directory") && !disk.contains("memory_"),
            "{disk}"
        );
        let catalog = full(Some(MemoryBudget::Catalog));
        assert!(
            catalog.contains("memory_catalog_max_bytes") && !catalog.contains("data directory"),
            "{catalog}"
        );
        let blobs = full(Some(MemoryBudget::Blobs));
        assert!(
            blobs.contains("memory_max_bytes") && !blobs.contains("memory_catalog_max_bytes"),
            "{blobs}"
        );
    }

    #[test]
    fn shared_renderer_preserves_complete_sorted_escaped_batches() {
        let errors = parsed(r#"{"version":1,"buckets":[{"name":"example-bucket","z\n\"secret-field":1,"storageClas":"secret-value","name":"bad name","name":"also bad"}]}"#).unwrap_err();
        let mut output = Vec::new();
        errors.write(&mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(!text.contains("secret-value"));
        let batch: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(batch["event"], "diagnostics");
        assert_eq!(batch["level"], "error");
        let records = batch["diagnostics"].as_array().unwrap();
        assert_eq!(records.len(), errors.errors.len());
        assert!(
            records
                .iter()
                .all(|record| record["source"] == "initializer")
        );
        assert!(
            records
                .windows(2)
                .all(|pair| pair[0]["key"].as_str() <= pair[1]["key"].as_str())
        );
        let repeated: Vec<_> = records
            .iter()
            .filter(|record| record["key"] == "buckets[0].name")
            .collect();
        assert!(
            repeated[0]["fix"]
                .as_str()
                .unwrap()
                .contains("occurrence 2")
        );
        assert!(
            repeated[1]["fix"]
                .as_str()
                .unwrap()
                .contains("occurrence 3")
        );
        assert!(records.iter().any(|record| {
            record["key"]
                .as_str()
                .unwrap()
                .contains("\\n\\\"secret-field")
        }));
    }

    #[test]
    fn existing_api_acceptance_and_legacy_profile_remain_separate() {
        let permissive_api = br#"{"name":"example-bucket","unknown":true,"lifecycle":{"rule":[]},"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":false}}}"#;
        assert!(
            parse_insert_with_profile(
                permissive_api,
                "local-project",
                FixedClock.now(),
                CompatProfile::Native,
                "US"
            )
            .is_ok()
        );
        let document = format!(
            r#"{{"version":1,"buckets":[{}]}}"#,
            std::str::from_utf8(permissive_api).unwrap()
        );
        assert_eq!(keys(&document).len(), 3);
        let legacy = br#"{"name":"ab"}"#;
        assert!(
            parse_insert_with_profile(
                legacy,
                "local-project",
                FixedClock.now(),
                CompatProfile::LegacyEmulator,
                "US"
            )
            .is_ok()
        );
        assert_eq!(
            keys(r#"{"version":1,"buckets":[{"name":"ab"}]}"#),
            ["buckets[0].name"]
        );
        for field in [
            "versioning",
            "labels",
            "name",
            "location",
            "storageClass",
            "defaultEventBasedHold",
        ] {
            let document =
                json!({"version":1,"buckets":[{"name":"example-bucket",field:null}]}).to_string();
            if field == "location" {
                assert_eq!(parsed(&document).unwrap()[0].location, "US");
            } else {
                assert!(parsed(&document).is_err(), "{field}");
            }
        }
        // Cloud Storage reads a null iamConfiguration as "no setting" (recorded 2026-10-31).
        let document =
            json!({"version":1,"buckets":[{"name":"example-bucket","iamConfiguration":null}]})
                .to_string();
        assert!(parsed(&document).is_ok());
    }

    #[test]
    fn regular_files_relative_paths_symlinks_and_read_failures() {
        let directory = crate::test_support::TestDir::new();
        let root = &directory.0;
        let file = root.join("buckets.json");
        fs::write(&file, r#"{"version":1,"buckets":[]}"#).unwrap();
        assert!(
            load(&file, "seed-project", &FixedClock, "US")
                .unwrap()
                .is_empty()
        );
        assert!(load(root, "seed-project", &FixedClock, "US").is_err());
        assert!(load(&root.join("missing"), "seed-project", &FixedClock, "US").is_err());
        let relative = Path::new("Cargo.toml");
        assert_eq!(
            load(relative, "seed-project", &FixedClock, "US")
                .unwrap_err()
                .errors[0]
                .key,
            "buckets_file"
        );
        let relative_directory = crate::test_support::TestDir::new_in(Path::new("."));
        let relative = relative_directory.0.join("buckets.json");
        fs::write(&relative, r#"{"version":1,"buckets":[]}"#).unwrap();
        assert!(load(&relative, "seed-project", &FixedClock, "US").is_ok());
        fs::remove_file(relative).unwrap();
        fs::write(&file, vec![b' '; FILE_LIMIT + 1]).unwrap();
        assert!(
            load(&file, "seed-project", &FixedClock, "US")
                .unwrap_err()
                .errors[0]
                .fix
                .contains("32 MiB")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let link = root.join("link.json");
            fs::write(&file, r#"{"version":1,"buckets":[]}"#).unwrap();
            symlink(&file, &link).unwrap();
            assert!(load(&link, "seed-project", &FixedClock, "US").is_ok());
            fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
            if std::fs::File::open(&file).is_err() {
                assert!(load(&file, "seed-project", &FixedClock, "US").is_err());
            } else {
                eprintln!(
                    "Unreadable-file assertion not applicable: this principal can open mode 000 files."
                );
            }
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
            let fifo = root.join("fifo");
            crate::test_support::fifo(&fifo);
            assert!(load(&fifo, "seed-project", &FixedClock, "US").is_err());
        }
    }
}
