#![deny(clippy::unwrap_used, clippy::expect_used)]

//! Versioned, side-effect-free bucket initialization input validation.

use crate::{
    bounded_file::{self, ReadError},
    bucket::BucketRecord,
    clock::Clock,
    config::CompatProfile,
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    json::{parse_insert_with_profile, validate_project},
};
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeOwned, MapAccess, Visitor},
};
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    time::SystemTime,
};

pub const FILE_LIMIT: usize = 1024 * 1024;
pub const ENTRY_LIMIT: usize = 64 * 1024;
pub const BUCKET_LIMIT: usize = 1000;
pub const DIAGNOSTIC_LIMIT: usize = 256;
pub const DIAGNOSTIC_BYTES_LIMIT: usize = 64 * 1024;
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
const MAX_INDEX_RANGES: usize = BUCKET_LIMIT.div_ceil(2);
const MAX_RANGE_BYTES: usize =
    "buckets[".len() + 2 * decimal_width(BUCKET_LIMIT - 1) + "..], ".len();
const MAX_REASON_BYTES: usize = REASON_COUNT * (REASON_LABEL_BYTES + decimal_width(usize::MAX) + 3);
const MAX_SUMMARY_RECORD_BYTES: usize =
    MAX_INDEX_RANGES * MAX_RANGE_BYTES + 3 * MAX_REASON_BYTES + 512;
const SUMMARY_ENVELOPE_BYTES: usize = 512;
const _: () = assert!(MAX_SUMMARY_RECORD_BYTES + SUMMARY_ENVELOPE_BYTES <= SUMMARY_BYTES_RESERVE);

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

struct Collected {
    root: Vec<Retained>,
    entries: Vec<Retained>,
    root_bytes: usize,
    entry_bytes: usize,
    total: usize,
    entry_seen: [bool; BUCKET_LIMIT],
    entry_retained: [bool; BUCKET_LIMIT],
    root_seen: BTreeSet<(RootKey, Reason)>,
    root_unknown_seen: BTreeSet<[u8; 32]>,
    groups: BTreeMap<([u8; 32], Reason), u8>,
    reasons: BTreeMap<&'static str, usize>,
    first_reasons: BTreeMap<&'static str, usize>,
    root_reasons: BTreeMap<&'static str, usize>,
}
impl Default for Collected {
    fn default() -> Self {
        Self {
            root: Vec::new(),
            entries: Vec::new(),
            root_bytes: 0,
            entry_bytes: 0,
            total: 0,
            entry_seen: [false; BUCKET_LIMIT],
            entry_retained: [false; BUCKET_LIMIT],
            root_seen: BTreeSet::new(),
            root_unknown_seen: BTreeSet::new(),
            groups: BTreeMap::new(),
            reasons: BTreeMap::new(),
            first_reasons: BTreeMap::new(),
            root_reasons: BTreeMap::new(),
        }
    }
}

impl Collected {
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
            .filter(|index| *index < BUCKET_LIMIT);
        let root_key = RootKey::parse(&key);
        let group = (group_digest(&key), reason);
        let priority = if let Some(index) = entry {
            let first = !self.entry_seen[index];
            self.entry_seen[index] = true;
            if first {
                *self.first_reasons.entry(reason.label()).or_default() += 1;
            }
            if self.entry_retained[index] {
                Priority::Extra
            } else {
                Priority::FirstKey
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
            let mut end = diagnostic.key.len().min(SHORT_KEY_BYTES);
            while !diagnostic.key.is_char_boundary(end) {
                end -= 1;
            }
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
                self.entry_retained[index] = true;
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
            let missing =
                std::array::from_fn(|index| self.entry_seen[index] && !self.entry_retained[index]);
            let summary = Diagnostic::new(
                DiagnosticSource::Initializer,
                "$",
                format!(
                    "Omitted {omitted} additional diagnostics (limits: {ARRAY_DIAGNOSTIC_LIMIT} details per normalized path/reason, {DIAGNOSTIC_LIMIT} records, {DIAGNOSTIC_BYTES_LIMIT} rendered bytes). Buckets without details: {}; first-error reasons: {}; omitted reasons: {}; root reasons: {}. Fix the reported fields and retry.",
                    entry_ranges(&missing),
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
            .map(|(index, record)| (record.priority, index))
            .collect();
        eligible.sort_by_key(|(priority, index)| (*priority, std::cmp::Reverse(*index)));
        for (_, index) in eligible {
            if records.len() - removed.len() < limit.records
                && *bytes - freed + candidate.bytes <= limit.bytes
            {
                break;
            }
            removed.push(index);
            freed += records[index].bytes;
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
        if character == '[' && chars.peek().is_some_and(|(_, c)| c.is_ascii_digit()) {
            let tail = &key[offset + 1..];
            let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
            if tail.as_bytes().get(digits) == Some(&b']') {
                let end = offset + digits + 2;
                while chars.peek().is_some_and(|(index, _)| *index < end) {
                    chars.next();
                }
                if offset == "buckets".len() && key.starts_with("buckets[") {
                    normalized.push_str(&key[offset..end]);
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

fn entry_ranges(seen: &[bool; BUCKET_LIMIT]) -> String {
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < seen.len() {
        if !seen[index] {
            index += 1;
            continue;
        }
        let first = index;
        while index + 1 < seen.len() && seen[index + 1] {
            index += 1;
        }
        if first == index {
            ranges.push(format!("buckets[{first}]"));
        } else {
            ranges.push(format!("buckets[{first}..{index}]"));
        }
        index += 1;
    }
    if ranges.is_empty() {
        "none".into()
    } else {
        ranges.join(", ")
    }
}

fn check_resource_size(bytes: &[u8]) -> Result<(), &'static str> {
    if bytes.len() > ENTRY_LIMIT {
        Err(
            "Encoded native resource exceeds 64 KiB; reduce it to at most 65,536 bytes after removing project.",
        )
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
    project: Option<String>,
    buckets: Vec<Entry>,
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
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = Members<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry()? {
                    members.push(member);
                }
                Ok(Members(members))
            }
        }
        deserializer.deserialize_map(Object)
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

fn duplicates(raw: &RawValue, key: &str, diagnostics: &mut Collected) {
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
                duplicates(raw, &field, diagnostics);
            }
        }
    } else if raw.get().starts_with('[')
        && let Some(items) = array(raw)
    {
        for (index, raw) in items
            .iter()
            .take(if key == "buckets" {
                BUCKET_LIMIT
            } else {
                usize::MAX
            })
            .enumerate()
        {
            duplicates(raw, &format!("{key}[{index}]"), diagnostics);
        }
    }
}

fn distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, b) in right.iter().enumerate() {
            let previous = row[j + 1];
            row[j + 1] = (diagonal + usize::from(a != *b))
                .min(row[j] + 1)
                .min(previous + 1);
            diagonal = previous;
        }
    }
    row[right.len()]
}

fn unsupported(name: &str) -> bool {
    matches!(
        name,
        "iam"
            | "iamPolicy"
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
            | "softDeletePolicy"
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

fn closed<T: DeserializeOwned>(fields: &Members<'_>, parent: &str, diagnostics: &mut Collected) {
    let names = schema_fields::<T>();
    for (name, _) in &fields.0 {
        // Derived closed structs are the schema authority; the names only supply advice.
        if names.contains(&name.as_str()) {
            continue;
        }
        let fix = if unsupported(name) {
            "Unsupported in bucket initialization version 1; remove this field, including empty or disabled settings.".into()
        } else if let Some(nearest) = names
            .iter()
            .min_by_key(|candidate| (distance(name, candidate), **candidate))
        {
            format!(
                "Unknown field; use {nearest}, for example {}.",
                example(nearest)
            )
        } else {
            "Unknown field; remove it.".into()
        };
        error(
            diagnostics,
            if unsupported(name) {
                Reason::Unsupported
            } else {
                Reason::Unknown
            },
            path(parent, name),
            fix,
        );
    }
}

fn example(field: &str) -> &'static str {
    match field {
        "version" => "\"version\":1",
        "buckets" => "\"buckets\":[]",
        "project" => "\"project\":\"local-project\"",
        "name" => "\"name\":\"example-bucket\"",
        "location" => "\"location\":\"US\"",
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
    let mut resource = json!({"name":"example-bucket"});
    resource[field] = value.clone();
    if parse_insert_with_profile(
        resource.to_string().as_bytes(),
        "local-project",
        now,
        CompatProfile::Native,
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

fn settings(
    fields: &Members<'_>,
    parent: &str,
    now: SystemTime,
    validate_values: bool,
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
            "project" => {
                project(raw, &key, diagnostics)?;
            }
            _ => {}
        }
    }
    if !validate_values {
        return Ok(());
    }
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
                        let key = format!("{key}[{index}]");
                        if let Some(rule) = members(raw) {
                            for (child, raw) in rule.0 {
                                if schema_fields::<Cors>().contains(&child.as_str()) {
                                    let values = value(raw)?;
                                    if let Some(items) =
                                        values.as_array().filter(|_| child != "maxAgeSeconds")
                                    {
                                        for (index, item) in items.iter().enumerate() {
                                            native(
                                                name,
                                                &json!([{&child: [item]}]),
                                                &format!("{}[{index}]", path(&key, &child)),
                                                now,
                                                diagnostics,
                                            );
                                        }
                                        // Empty arrays and scalar-only fields still need type checks.
                                        if items.is_empty() {
                                            native(
                                                name,
                                                &json!([{&child: values}]),
                                                &path(&key, &child),
                                                now,
                                                diagnostics,
                                            );
                                        }
                                    } else {
                                        native(
                                            name,
                                            &json!([{&child: values}]),
                                            &path(&key, &child),
                                            now,
                                            diagnostics,
                                        );
                                    }
                                }
                            }
                        } else {
                            native(name, &json!([value(raw)?]), &key, now, diagnostics);
                        }
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
            _ => {}
        }
    }
    Ok(())
}

/// Read one selected regular file; relative paths use the process working directory.
pub fn load(
    file: &Path,
    seed_project: &str,
    clock: &dyn Clock,
) -> Result<Vec<BucketRecord>, Diagnostics> {
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
    let bytes = bounded_file::read_from(file, FILE_LIMIT as u64).map_err(|error| Diagnostics::error(
        DiagnosticSource::Initializer, "buckets_file", match error {
            ReadError::Limit => "Bucket initialization file exceeds 1 MiB; reduce it to at most 1,048,576 bytes.",
            ReadError::Io(_) => "Cannot read bucket initialization file; select a readable local JSON file, for example buckets.json.",
        }))?;
    parse(&bytes, seed_project, clock)
}

/// Validate the entire document before returning any records, always using native semantics.
pub fn parse(
    bytes: &[u8],
    seed_project: &str,
    clock: &dyn Clock,
) -> Result<Vec<BucketRecord>, Diagnostics> {
    let failure = |key, fix| Diagnostics::error(DiagnosticSource::Initializer, key, fix);
    if bytes.len() > FILE_LIMIT {
        return Err(failure(
            "buckets_file",
            "Bucket initialization file exceeds 1 MiB; reduce it to at most 1,048,576 bytes.",
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
    let mut diagnostics = Collected::default();
    duplicates(raw, "", &mut diagnostics);
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
        if entries.len() > BUCKET_LIMIT {
            error(
                &mut diagnostics,
                Reason::Limit,
                "buckets",
                format!(
                    "Too many buckets; keep at most 1000 entries. Remaining {} entries were not inspected.",
                    entries.len() - BUCKET_LIMIT
                ),
            );
        }
        if !version_valid {
            continue;
        }
        for (index, raw) in entries.iter().take(BUCKET_LIMIT).enumerate() {
            let key = format!("buckets[{index}]");
            let raw_oversized = raw.get().len() > ENTRY_LIMIT;
            if raw_oversized {
                error(
                    &mut diagnostics,
                    Reason::Limit,
                    &key,
                    "Entry exceeds 64 KiB; reduce its original JSON span, including project and whitespace, to at most 65,536 bytes.",
                );
            }
            let Some(fields) = members(raw) else {
                error(
                    &mut diagnostics,
                    Reason::Shape,
                    &key,
                    "Expected a bucket object; use {\"name\":\"example-bucket\"}.",
                );
                continue;
            };
            let mut resource = value(raw)?;
            for (_, raw) in fields.0.iter().filter(|(field, _)| field == "name") {
                if let Some(name) = value(raw)?.as_str() {
                    if let Some(first) = names.get(name) {
                        if *first != index {
                            error(
                                &mut diagnostics,
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
            if let Some(object) = resource.as_object_mut() {
                object.remove("project");
            }
            let encoded = resource.to_string();
            let encoded_limit = check_resource_size(encoded.as_bytes());
            let encoded_oversized = encoded_limit.is_err();
            if let Err(fix) = encoded_limit {
                error(&mut diagnostics, Reason::Limit, &key, fix);
            }
            settings(
                &fields,
                &key,
                now,
                !raw_oversized && !encoded_oversized,
                &mut diagnostics,
            )?;
        }
    }
    if diagnostics.is_err() {
        return Err(diagnostics.finish());
    }
    let document: Document = serde_json::from_slice(bytes).map_err(|_| {
        failure(
            "$",
            "Invalid initializer schema; use version 1 with native bucket resources.",
        )
    })?;
    let inherited = document.project.as_deref().unwrap_or(seed_project);
    let mut records = Vec::with_capacity(document.buckets.len());
    for (index, mut entry) in document.buckets.into_iter().enumerate() {
        let project = entry.project.take().unwrap_or_else(|| inherited.to_owned());
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
                fix,
            );
            continue;
        }
        match parse_insert_with_profile(&encoded, &project, now, CompatProfile::Native) {
            Ok(record) => records.push(record),
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use proptest::prelude::*;
    use std::{
        fs,
        time::{Duration, UNIX_EPOCH},
    };

    struct FixedClock;
    impl Clock for FixedClock {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_micros(1_700_000_000_123_456)
        }
    }

    fn parsed(document: &str) -> Result<Vec<BucketRecord>, Diagnostics> {
        parse(document.as_bytes(), "seed-project", &FixedClock)
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
            let result = parse(&bytes, "seed-project", &FixedClock);
            if let Err(errors) = result {
                prop_assert!(errors.is_err());
                let mut output = Vec::new();
                errors.write(&mut output).unwrap();
                prop_assert!(output.len() <= DIAGNOSTIC_BYTES_LIMIT);
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
            let document = json!({"version":1,"buckets":[resource.clone()]});
            match parsed(&document.to_string()) {
                Ok(records) => { prop_assert_eq!(records.len(), 1); prop_assert_eq!(records[0].name.as_str(), resource["name"].as_str().unwrap()); }
                Err(errors) => { prop_assert!(errors.errors.iter().all(|record| record.key != "$" || record.fix.starts_with("Omitted")), "{errors:?}"); }
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
        assert!(errors.errors.iter().any(|record| record.key == "version" && record.fix.contains("Unsupported version")));
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.key == "buckets" && record.fix.contains("Expected an array"))
        );
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
        assert!(
            errors
                .errors
                .iter()
                .any(|record| record.key == "bucket" && record.fix.contains("use buckets,"))
        );
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
            let entries = (0..BUCKET_LIMIT).map(|index| json!({"name":if !alternating || index % 2 == 0 { format!("bad name-{index}") } else { format!("bucket-{index:04}") }})).collect::<Vec<_>>();
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
            assert!(errors.errors.len() <= DIAGNOSTIC_LIMIT);
            let mut rendered = Vec::new();
            errors.write(&mut rendered).unwrap();
            assert!(rendered.len() <= DIAGNOSTIC_BYTES_LIMIT);
        }
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
        let values = (0..3400)
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
        let mut entries = (0..BUCKET_LIMIT)
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
        assert_eq!(records[0].storage_class, "NEARLINE");
        assert_eq!(records[0].labels["env"], "dev");
        assert_eq!(records[0].cors[0].max_age_seconds, Some(3600));
        assert!(records[0].versioning_enabled);
        assert!(records[0].default_event_based_hold);
        assert!(records[0].public_access_prevention_enforced);
        let defaults = parsed(r#"{"version":1,"buckets":[{"name":"default-bucket","cors":null,"versioning":{},"iamConfiguration":{}}]}"#).unwrap();
        assert_eq!(defaults[0].project, "seed-project");
        assert_eq!(defaults[0].location, "US");
        assert_eq!(defaults[0].storage_class, "STANDARD");
        assert!(!defaults[0].versioning_enabled);
        assert!(!defaults[0].default_event_based_hold);
        assert!(!defaults[0].public_access_prevention_enforced);
        assert!(parsed(r#"{"version":1,"buckets":[]}"#).unwrap().is_empty());
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
            let errors = parse(bytes, "seed-project", &FixedClock).unwrap_err();
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
                    CompatProfile::Native
                )
                .is_ok()
            );
            if extra == 0 {
                assert!(check_resource_size(entry.as_bytes()).is_ok());
                assert!(parsed(&format!(r#"{{"version":1,"buckets":[{entry}]}}"#)).is_ok());
            } else {
                assert_eq!(
                    check_resource_size(entry.as_bytes()).unwrap_err(),
                    "Encoded native resource exceeds 64 KiB; reduce it to at most 65,536 bytes after removing project."
                );
            }
        }
        assert_eq!(
            keys(
                r#"{"version":1,"buckets":[{"name":"example-bucket","cors":[{"maxAgeSeconds":1e3}]}]}"#
            ),
            ["buckets[0].cors[0].maxAgeSeconds"]
        );
        let labels = (0..3400)
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
        assert!(
            errors
                .errors
                .iter()
                .any(|error| error.key == "buckets[0]" && error.fix.contains("Encoded"))
        );
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
                &FixedClock
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
            ("iamPolicy", json!({"bindings":[]})),
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
        let mut errors = Collected::default();
        closed::<Document>(&members(raw).unwrap(), "", &mut errors);
        assert!(errors.finish().errors[0].fix.contains("use buckets,"));
    }

    #[test]
    fn repeated_members_at_every_depth_and_decoded_global_names_are_reported() {
        let errors = parsed(r#"{"version":1,"buckets":[{"name":"first-bucket","name":"second-bucket"},{"name":"first-bucket"}]}"#).unwrap_err();
        assert!(
            errors.errors.iter().any(
                |error| error.key == "buckets[1].name" && error.fix.contains("buckets[0].name")
            )
        );
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
                CompatProfile::Native
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
                CompatProfile::LegacyEmulator
            )
            .is_ok()
        );
        assert_eq!(
            keys(r#"{"version":1,"buckets":[{"name":"ab"}]}"#),
            ["buckets[0].name"]
        );
        for field in [
            "versioning",
            "iamConfiguration",
            "labels",
            "name",
            "location",
            "storageClass",
            "defaultEventBasedHold",
        ] {
            let document =
                json!({"version":1,"buckets":[{"name":"example-bucket",field:null}]}).to_string();
            assert!(parsed(&document).is_err(), "{field}");
        }
    }

    #[test]
    fn regular_files_relative_paths_symlinks_and_read_failures() {
        let directory = crate::test_support::TestDir::new();
        let root = &directory.0;
        let file = root.join("buckets.json");
        fs::write(&file, r#"{"version":1,"buckets":[]}"#).unwrap();
        assert!(load(&file, "seed-project", &FixedClock).unwrap().is_empty());
        assert!(load(root, "seed-project", &FixedClock).is_err());
        assert!(load(&root.join("missing"), "seed-project", &FixedClock).is_err());
        let relative = Path::new("Cargo.toml");
        assert_eq!(
            load(relative, "seed-project", &FixedClock)
                .unwrap_err()
                .errors[0]
                .key,
            "buckets_file"
        );
        let relative_directory = crate::test_support::TestDir::new_in(Path::new("."));
        let relative = relative_directory.0.join("buckets.json");
        fs::write(&relative, r#"{"version":1,"buckets":[]}"#).unwrap();
        assert!(load(&relative, "seed-project", &FixedClock).is_ok());
        fs::remove_file(relative).unwrap();
        fs::write(&file, vec![b' '; FILE_LIMIT + 1]).unwrap();
        assert!(
            load(&file, "seed-project", &FixedClock).unwrap_err().errors[0]
                .fix
                .contains("1 MiB")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let link = root.join("link.json");
            fs::write(&file, r#"{"version":1,"buckets":[]}"#).unwrap();
            symlink(&file, &link).unwrap();
            assert!(load(&link, "seed-project", &FixedClock).is_ok());
            fs::set_permissions(&file, fs::Permissions::from_mode(0o0)).unwrap();
            if std::fs::File::open(&file).is_err() {
                assert!(load(&file, "seed-project", &FixedClock).is_err());
            } else {
                eprintln!(
                    "Unreadable-file assertion not applicable: this principal can open mode 000 files."
                );
            }
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
            let fifo = root.join("fifo");
            crate::test_support::fifo(&fifo);
            assert!(load(&fifo, "seed-project", &FixedClock).is_err());
        }
    }
}
