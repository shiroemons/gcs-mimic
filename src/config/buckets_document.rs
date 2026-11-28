//! The buckets document that `buckets_file` or `buckets` supplies: its size limit, its read from
//! a file and its parse as one JSON value, which startup does once, before authentication and
//! the bucket initializer use it. The initializer validates the entries and applies them.

pub(crate) mod shorthand;

use super::{DocumentKind, DocumentSource, Form};
use crate::{
    bounded_file::{self, ReadError},
    diagnostics::{DiagnosticSource, Diagnostics},
};
use serde_json::Value;
use std::{collections::BTreeSet, num::NonZeroUsize, path::Path, sync::Arc};

/// Leaves room beside one entry of the largest size, which a smaller file limit would refuse first.
pub(crate) const FILE_LIMIT: usize = 2 * ENTRY_LIMIT;
/// What each allowed bucket adds to a file: a fixed size would otherwise refuse a file long before
/// it reached the configured bucket limit.
pub(crate) const FILE_BYTES_PER_BUCKET: usize = 1024;
/// The whole file is read into memory and parsed, so even without a bucket limit a mistaken path
/// to some huge file must fail here. A million buckets of 1 KiB each still fit.
pub(crate) const UNLIMITED_FILE_LIMIT: usize = 1024 * 1024 * 1024;
pub(crate) const ENTRY_LIMIT: usize = crate::bounded_body::RESOURCE_BODY_LIMIT;

/// The document limit, which applies to a file and to inline text alike.
pub(crate) fn file_limit(bucket_limit: Option<NonZeroUsize>) -> usize {
    bucket_limit.map_or(UNLIMITED_FILE_LIMIT, |limit| {
        limit
            .get()
            .saturating_mul(FILE_BYTES_PER_BUCKET)
            .max(FILE_LIMIT)
    })
}

/// What a document of `form` is called in a fix.
fn noun(form: Form) -> &'static str {
    match form {
        Form::File => "file",
        Form::Inline => "document",
    }
}

pub(crate) fn file_limit_fix(form: Form, bucket_limit: Option<NonZeroUsize>) -> String {
    let limit = file_limit(bucket_limit);
    let noun = noun(form);
    match (form, bucket_limit) {
        (Form::File | Form::Inline, Some(_)) => format!(
            "Bucket initialization {noun} exceeds {limit} bytes; reduce it, or raise max_buckets_per_batch, which allows 1 KiB per bucket and at least 32 MiB in all."
        ),
        (Form::File, None) => format!(
            "Bucket initialization file exceeds {limit} bytes; check that buckets_file names the intended file, or reduce it."
        ),
        (Form::Inline, None) => {
            format!("Bucket initialization document exceeds {limit} bytes; reduce it.")
        }
    }
}

/// A buckets document that startup read and parsed as one JSON value before the initializer's
/// phase. Strict mode loads authentication first, and its built-in identities hold their
/// implicit roles in the projects this parse finds; the initializer then validates these bytes
/// instead of reading the file again. Both therefore see one version of a file that may change
/// while the server starts, and a document of up to 1 GiB is read and parsed as one value once.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ParsedDocument {
    form: Form,
    /// One JSON value within the document limit; shared, because a configuration copy keeps it.
    bytes: Arc<[u8]>,
    /// The project of each bucket the document creates where the document names one: the
    /// entry's `project`, or else the document's. Any other entry inherits `seed_project`.
    projects: BTreeSet<String>,
}

impl ParsedDocument {
    pub(crate) fn form(&self) -> Form {
        self.form
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// With `seed_project`, the projects of the buckets the document creates. For a document
    /// that validates, these are exactly the buckets' projects; one that does not stops startup
    /// before they matter.
    pub(crate) fn projects(&self) -> &BTreeSet<String> {
        &self.projects
    }
}

/// Reads the document that `source` names and parses it as one JSON value, failing with the
/// diagnostic that the initializer gives an unreadable, oversized or malformed document.
pub(crate) fn parse_document(
    source: &DocumentSource,
    bucket_limit: Option<NonZeroUsize>,
) -> Result<ParsedDocument, Diagnostics> {
    let (form, bytes): (Form, Arc<[u8]>) = match source {
        DocumentSource::File(path) => (Form::File, read_file(path, bucket_limit)?.into()),
        DocumentSource::Inline(text) => (Form::Inline, Arc::clone(text).into()),
        DocumentSource::Parsed(document) => return Ok(document.clone()),
    };
    let document = json_value(&bytes, form, bucket_limit)?;
    // The same inheritance that `bucket_init::parse` applies to the records it builds.
    let inherited = document.get("project").and_then(Value::as_str);
    let projects = document
        .get("buckets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("project").and_then(Value::as_str).or(inherited))
        .map(str::to_owned)
        .collect();
    Ok(ParsedDocument {
        form,
        bytes,
        projects,
    })
}

pub(crate) fn read_file(
    file: &Path,
    bucket_limit: Option<NonZeroUsize>,
) -> Result<Vec<u8>, Diagnostics> {
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
    bounded_file::read_from(file, file_limit(bucket_limit) as u64).map_err(|error| Diagnostics::error(
        DiagnosticSource::Initializer, "buckets_file", match error {
            ReadError::Limit => file_limit_fix(Form::File, bucket_limit),
            ReadError::Io(_) => "Cannot read bucket initialization file; select a readable local JSON file, for example buckets.json.".into(),
        }))
}

/// The document's size and its parse as one JSON value, which every later check relies on.
pub(crate) fn json_value(
    bytes: &[u8],
    form: Form,
    bucket_limit: Option<NonZeroUsize>,
) -> Result<Value, Diagnostics> {
    let setting = DocumentKind::Buckets.setting(form).as_str();
    if bytes.len() > file_limit(bucket_limit) {
        return Err(Diagnostics::error(
            DiagnosticSource::Initializer,
            setting,
            file_limit_fix(form, bucket_limit),
        ));
    }
    // Value enforces UTF-8, finite numbers and serde's recursion bound before raw traversal.
    serde_json::from_slice(bytes).map_err(|_| Diagnostics::error(DiagnosticSource::Initializer, setting, format!("Malformed bucket initialization {}; use one UTF-8 JSON object without BOM, comments or trailing data, for example {{\"version\":1,\"buckets\":[]}}.", noun(form))))
}
