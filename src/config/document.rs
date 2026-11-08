//! Settings that supply a JSON document, either as a path to a file or as the document text.
//!
//! `identity_file` and `identities` supply the identity registry; `buckets_file` and `buckets`
//! supply bucket initialization. Both forms load and validate the same document, so they differ
//! only in where the text comes from and in the setting that diagnostics name.

use super::{Config, ConfigurationReport, Diagnostic, Setting, SettingFix, Settings};
use serde_json::value::RawValue;
use std::{fmt, path::PathBuf};

/// The fix for a config file member that holds neither a document object nor a string.
pub(super) const MEMBER_FIX: &str =
    "Expected the document as a JSON object or as a string that contains it.";

/// Where a document's JSON text comes from.
#[derive(Clone, PartialEq, Eq)]
pub enum DocumentSource {
    /// A path the document is read from when it loads; a relative one names the working directory.
    File(PathBuf),
    /// The document text itself.
    Inline(String),
}

impl DocumentSource {
    pub(crate) fn form(&self) -> Form {
        match self {
            Self::File(_) => Form::File,
            Self::Inline(_) => Form::Inline,
        }
    }
}

/// An identity document holds bearer tokens, so debug output shows only how much text there is.
impl fmt::Debug for DocumentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => f.debug_tuple("File").field(path).finish(),
            Self::Inline(text) => write!(f, "Inline(<{} bytes>)", text.len()),
        }
    }
}

/// Which of a document's two settings supplied it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Form {
    File,
    Inline,
}

/// A document that either a file setting or an inline setting supplies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DocumentKind {
    Identities,
    Buckets,
}

impl DocumentKind {
    const ALL: [Self; 2] = [Self::Identities, Self::Buckets];

    /// The setting that supplies this document in `form`, which diagnostics name.
    pub(crate) fn setting(self, form: Form) -> Setting {
        match (self, form) {
            (Self::Identities, Form::File) => Setting::IdentityFile,
            (Self::Identities, Form::Inline) => Setting::Identities,
            (Self::Buckets, Form::File) => Setting::BucketsFile,
            (Self::Buckets, Form::Inline) => Setting::Buckets,
        }
    }

    fn noun(self) -> &'static str {
        match self {
            Self::Identities => "identity registry",
            Self::Buckets => "bucket initialization document",
        }
    }

    /// The field of `config` that holds this document.
    fn source(self, config: &mut Config) -> &mut Option<DocumentSource> {
        match self {
            Self::Identities => &mut config.identities,
            Self::Buckets => &mut config.buckets,
        }
    }

    /// Records the path or text that `form`'s setting names.
    pub(super) fn apply(
        self,
        config: &mut Config,
        form: Form,
        value: &str,
    ) -> Result<(), SettingFix> {
        if value.is_empty() {
            return Err(self.empty_fix(form).into());
        }
        *self.source(config) = Some(match form {
            Form::File => DocumentSource::File(PathBuf::from(value)),
            Form::Inline => DocumentSource::Inline(value.to_owned()),
        });
        Ok(())
    }

    fn empty_fix(self, form: Form) -> &'static str {
        match (self, form) {
            (Self::Identities, Form::File) => "identity_file must not be empty",
            (Self::Identities, Form::Inline) => {
                "identities must not be empty; supply the identity registry JSON document or omit the setting."
            }
            (Self::Buckets, Form::File) => {
                "buckets_file must not be empty; use buckets.json or omit the setting"
            }
            (Self::Buckets, Form::Inline) => {
                "buckets must not be empty; supply the bucket initialization JSON document, for example {\"version\":1,\"buckets\":[]}, or omit the setting."
            }
        }
    }
}

/// Whether a config file member named `key` may hold a document instead of a string.
pub(super) fn holds_document(key: &str) -> bool {
    DocumentKind::ALL
        .into_iter()
        .any(|kind| kind.setting(Form::Inline).as_str() == key)
}

/// The document text a config file member supplies: the object as written, so that repeated
/// members stay visible to the document's own checks, or the contents of a string.
pub(super) fn member_text(raw: &RawValue) -> Option<String> {
    let text = raw.get();
    if text.starts_with('{') {
        Some(text.to_owned())
    } else {
        serde_json::from_str(text).ok()
    }
}

impl ConfigurationReport {
    /// A document has one source, so naming both of its settings is a conflict rather than a
    /// precedence to resolve. Both settings become invalid, so nothing loads either one.
    pub(super) fn check_document_sources(&mut self, settings: &Settings) {
        for kind in DocumentKind::ALL {
            let file = kind.setting(Form::File).as_str();
            let inline = kind.setting(Form::Inline).as_str();
            if settings.contains_key(file)
                && let Some(input) = settings.get(inline)
            {
                self.diagnostics.errors.push(Diagnostic::new(
                    input.source,
                    &input.key,
                    format!(
                        "{inline} and {file} both supply the {}; keep only one of them.",
                        kind.noun()
                    ),
                ));
                self.invalid.extend([file, inline].map(str::to_owned));
            }
        }
    }
}

#[cfg(test)]
mod tests;
