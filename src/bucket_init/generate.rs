//! Writes a buckets document from answers that `gcs-mimic buckets new` collects from flags or
//! prompts. Both build [`Bucket`] values through [`Bucket::apply`], so answers given either way
//! produce the same bytes, and every document passes the startup parser before it is written.
//!
//! The parser's types read documents and cannot write them, so the entry here is its own
//! write-side shape. It names only members the document format accepts, and only the members an
//! answer chose: a bucket without answers is a name-only entry.

use super::{Form, parse};
use crate::{clock::SystemClock, config::Config, diagnostics::Diagnostics};
use serde::{Serialize, Serializer};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    str::FromStr,
};

crate::config::string_enum! {
    no_default listed
    /// Where the document goes: a `buckets_file` (`json`), a `.env` file or shell line (`env`),
    /// or a Compose service's `environment:` (`compose`).
    pub enum Format {
        Json => "json",
        Env => "env",
        Compose => "compose",
    }
    "format must be json, env or compose"
}

crate::config::string_enum! {
    no_default listed
    /// The entry's `iamConfiguration.publicAccessPrevention`.
    pub enum PublicAccessPrevention {
        Inherited => "inherited",
        Enforced => "enforced",
    }
    "public access prevention must be inherited or enforced"
}

crate::serde_util::serialize_as_str!(PublicAccessPrevention);

/// The role that public read grants to `allUsers`.
const PUBLIC_READ_ROLE: &str = "roles/storage.objectViewer";

/// The storage classes a prompt offers. The legacy `MULTI_REGIONAL` and
/// `DURABLE_REDUCED_AVAILABILITY` stay valid in a document but are not offered for new buckets.
pub fn storage_classes() -> [&'static str; 4] {
    use crate::bucket::StorageClass;
    [
        StorageClass::Standard,
        StorageClass::Nearline,
        StorageClass::Coldline,
        StorageClass::Archive,
    ]
    .map(StorageClass::as_str)
}

/// The multi-regions a prompt lists before it asks for any other location.
pub fn multi_regions() -> &'static [&'static str] {
    crate::bucket::LocationKind::MultiRegion.names()
}

/// The canonical name of a supported location, matched without regard to case as startup does.
pub fn location(value: &str) -> Option<&'static str> {
    value
        .parse::<crate::bucket::Location>()
        .ok()
        .map(crate::bucket::Location::as_str)
}

/// The predefined roles a bucket policy can grant; the project-level ones are left out.
pub fn bucket_roles() -> Vec<&'static str> {
    use crate::auth::roles::{GrantLevel, Role};
    Role::ALL
        .iter()
        .filter(|role| match role.level() {
            GrantLevel::Project => false,
            GrantLevel::Bucket | GrantLevel::ManagedFolder => true,
        })
        .map(|role| role.as_str())
        .collect()
}

/// A label written `key=value` that startup accepts. It is checked when it is read, so that a
/// prompt can ask again at once instead of losing every answer to the parser's check of the
/// whole document, which checks it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    key: String,
    value: String,
}

impl FromStr for Label {
    type Err = GenerateError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        use crate::bucket::{LabelPart, valid_label};
        let (key, value) = text.split_once('=').ok_or(GenerateError::UnpairedLabel)?;
        if !valid_label(key, LabelPart::Key) || !valid_label(value, LabelPart::Value) {
            return Err(GenerateError::InvalidLabel);
        }
        Ok(Self {
            key: key.to_owned(),
            value: value.to_owned(),
        })
    }
}

/// One answer about one bucket, naming one member of its entry. Single-valued answers may be
/// given once per bucket; labels, CORS lists and IAM bindings accumulate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setting {
    Project(String),
    Location(String),
    StorageClass(String),
    Label(Label),
    Versioning,
    DefaultEventBasedHold,
    PublicAccessPrevention(PublicAccessPrevention),
    SoftDeleteRetentionSeconds(u64),
    CorsOrigin(String),
    CorsMethod(String),
    CorsResponseHeader(String),
    CorsMaxAgeSeconds(u64),
    /// Grants `allUsers` object reads through `iamPolicy`.
    PublicRead,
    IamBinding {
        role: String,
        members: Vec<String>,
    },
}

impl Setting {
    /// The entry member the answer writes, which a refused repeat names.
    fn member(&self) -> &'static str {
        match self {
            Self::Project(_) => "project",
            Self::Location(_) => "location",
            Self::StorageClass(_) => "storageClass",
            Self::Label(_) => "labels",
            Self::Versioning => "versioning",
            Self::DefaultEventBasedHold => "defaultEventBasedHold",
            Self::PublicAccessPrevention(_) => "iamConfiguration.publicAccessPrevention",
            Self::SoftDeleteRetentionSeconds(_) => "softDeletePolicy.retentionDurationSeconds",
            Self::CorsOrigin(_) => "cors.origin",
            Self::CorsMethod(_) => "cors.method",
            Self::CorsResponseHeader(_) => "cors.responseHeader",
            Self::CorsMaxAgeSeconds(_) => "cors.maxAgeSeconds",
            Self::PublicRead => "public read",
            Self::IamBinding { .. } => "iamPolicy.bindings",
        }
    }
}

/// One entry of the document, in the order its members are written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_class: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "one_rule")]
    cors: Option<Cors>,
    #[serde(skip_serializing_if = "Option::is_none")]
    versioning: Option<Versioning>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    default_event_based_hold: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    iam_configuration: Option<IamConfiguration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    soft_delete_policy: Option<SoftDeletePolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iam_policy: Option<IamPolicy>,
    /// Kept apart from the bindings so that public read cannot be asked for twice.
    #[serde(skip)]
    public_read: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Cors {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    origin: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    method: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    response_header: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_age_seconds: Option<u64>,
}

/// The answers describe one CORS rule, which the document holds in a list.
#[expect(
    clippy::ref_option,
    reason = "serde's serialize_with passes the field by reference"
)]
fn one_rule<S: Serializer>(rule: &Option<Cors>, serializer: S) -> Result<S::Ok, S::Error> {
    rule.as_slice().serialize(serializer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct Versioning {
    enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct IamConfiguration {
    public_access_prevention: PublicAccessPrevention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct SoftDeletePolicy {
    retention_duration_seconds: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
struct IamPolicy {
    bindings: Vec<Binding>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Binding {
    role: String,
    members: Vec<String>,
}

impl Bucket {
    /// A name-only entry. The name is checked here so that a prompt can ask again at once; the
    /// startup parser checks it again with the whole document.
    pub fn new(name: &str) -> Result<Self, GenerateError> {
        crate::bucket::name::validate(name).map_err(|_| GenerateError::InvalidName)?;
        Ok(Self {
            name: name.to_owned(),
            project: None,
            location: None,
            storage_class: None,
            labels: BTreeMap::new(),
            cors: None,
            versioning: None,
            default_event_based_hold: false,
            iam_configuration: None,
            soft_delete_policy: None,
            iam_policy: None,
            public_read: false,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this entry enforces public access prevention, under which startup refuses a
    /// public grant.
    fn prevents_public_access(&self) -> bool {
        self.iam_configuration.is_some_and(|configuration| {
            configuration.public_access_prevention == PublicAccessPrevention::Enforced
        })
    }

    /// Startup plans each entry against the catalog and refuses a public grant under prevention
    /// there, after parsing, so the parser alone does not catch this entry's own conflict. A
    /// project's prevention is still only known at startup.
    fn grants_public_access_under_prevention(&self) -> bool {
        self.prevents_public_access()
            && self.iam_policy.as_ref().is_some_and(|policy| {
                policy.bindings.iter().any(|binding| {
                    binding.members.iter().any(|member| {
                        matches!(member.as_str(), "allUsers" | "allAuthenticatedUsers")
                    })
                })
            })
    }

    /// Records one answer, refusing a second value for a single-valued member.
    pub fn apply(&mut self, setting: Setting) -> Result<(), GenerateError> {
        let repeated = GenerateError::Repeated(setting.member());
        let fresh = match setting {
            Setting::Project(project) => set_once(&mut self.project, project),
            Setting::Location(location) => set_once(&mut self.location, location),
            Setting::StorageClass(class) => set_once(&mut self.storage_class, class),
            Setting::Label(Label { key, value }) => match self.labels.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(value);
                    true
                }
                Entry::Occupied(_) => false,
            },
            Setting::Versioning => set_once(&mut self.versioning, Versioning { enabled: true }),
            Setting::DefaultEventBasedHold => {
                !std::mem::replace(&mut self.default_event_based_hold, true)
            }
            Setting::PublicAccessPrevention(prevention) => set_once(
                &mut self.iam_configuration,
                IamConfiguration {
                    public_access_prevention: prevention,
                },
            ),
            Setting::SoftDeleteRetentionSeconds(seconds) => set_once(
                &mut self.soft_delete_policy,
                SoftDeletePolicy {
                    retention_duration_seconds: seconds,
                },
            ),
            Setting::CorsOrigin(origin) => push(&mut self.cors_rule().origin, origin),
            Setting::CorsMethod(method) => push(&mut self.cors_rule().method, method),
            Setting::CorsResponseHeader(header) => {
                push(&mut self.cors_rule().response_header, header)
            }
            Setting::CorsMaxAgeSeconds(seconds) => {
                set_once(&mut self.cors_rule().max_age_seconds, seconds)
            }
            Setting::PublicRead => {
                let fresh = !std::mem::replace(&mut self.public_read, true);
                if fresh {
                    self.bind(PUBLIC_READ_ROLE.to_owned(), vec!["allUsers".to_owned()]);
                }
                fresh
            }
            Setting::IamBinding { role, members } => {
                self.bind(role, members);
                true
            }
        };
        if fresh { Ok(()) } else { Err(repeated) }
    }

    fn cors_rule(&mut self) -> &mut Cors {
        self.cors.get_or_insert_with(Cors::default)
    }

    fn bind(&mut self, role: String, members: Vec<String>) {
        self.iam_policy
            .get_or_insert_with(IamPolicy::default)
            .bindings
            .push(Binding { role, members });
    }
}

/// Stores `value` unless the member already has one; returns whether it stored.
fn set_once<T>(member: &mut Option<T>, value: T) -> bool {
    if member.is_some() {
        return false;
    }
    *member = Some(value);
    true
}

/// Adds a list value; a list member accepts any number of them.
fn push(list: &mut Vec<String>, value: String) -> bool {
    list.push(value);
    true
}

/// A complete buckets document.
#[derive(Debug, Serialize)]
pub struct Document {
    version: u8,
    buckets: Vec<Bucket>,
}

impl Document {
    pub fn new(buckets: Vec<Bucket>) -> Self {
        Self {
            version: 1,
            buckets,
        }
    }

    /// The document as `format` writes it, after the text the server will read passes the
    /// startup parser with the default `seed_project` and `default_location`. Checks that depend on
    /// the catalog, such as a project's public access prevention, still happen at startup.
    pub fn render(&self, format: Format) -> Result<String, GenerateError> {
        if let Some(index) = self
            .buckets
            .iter()
            .position(Bucket::grants_public_access_under_prevention)
        {
            return Err(GenerateError::PublicUnderPrevention { index });
        }
        let text = match format {
            Format::Env => serde_json::to_string(self),
            Format::Json | Format::Compose => serde_json::to_string_pretty(self),
        }
        .map_err(GenerateError::Encode)?;
        let defaults = Config::default();
        let form = match format {
            Format::Json => Form::File,
            Format::Env | Format::Compose => Form::Inline,
        };
        parse(
            text.as_bytes(),
            form,
            &defaults.seed_project,
            &SystemClock,
            &defaults.default_location,
            None,
        )
        .map_err(GenerateError::Rejected)?;
        match format {
            Format::Json => Ok(format!("{text}\n")),
            Format::Env => env_line(&text),
            Format::Compose => Ok(compose_snippet(&text)),
        }
    }
}

/// One `.env` line. Single quotes keep Compose and shells from expanding `$`, and neither form
/// can escape a quote inside them.
fn env_line(text: &str) -> Result<String, GenerateError> {
    if text.contains('\'') {
        return Err(GenerateError::Unquotable);
    }
    Ok(format!("GCS_MIMIC_BUCKETS='{text}'\n"))
}

/// An `environment:` block to paste into a service. Compose interpolates `$` in values, and
/// `$$` is its literal dollar.
fn compose_snippet(text: &str) -> String {
    let mut snippet = String::from("environment:\n  GCS_MIMIC_BUCKETS: |\n");
    for line in text.replace('$', "$$").lines() {
        snippet.push_str("    ");
        snippet.push_str(line);
        snippet.push('\n');
    }
    snippet
}

/// Why an answer or a document was refused.
#[derive(Debug)]
pub enum GenerateError {
    /// The name breaks the bucket-name rules.
    InvalidName,
    /// A label has no `=` between its key and value.
    UnpairedLabel,
    /// A label's key or value breaks the label rules.
    InvalidLabel,
    /// A single-valued member was given twice for one bucket.
    Repeated(&'static str),
    /// The entry at `index` grants public access while it enforces public access prevention,
    /// which startup refuses.
    PublicUnderPrevention {
        index: usize,
    },
    /// The startup parser refused the document; its diagnostics name each entry.
    Rejected(Diagnostics),
    /// The env format cannot quote a `'` in the document.
    Unquotable,
    Encode(serde_json::Error),
}

impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName => f.write_str(
                "Bucket names use lower-case letters, digits, '-', '_' and '.', 3 to 63 characters (222 with dots), and start and end with a letter or digit, for example app-uploads.",
            ),
            Self::UnpairedLabel => {
                f.write_str("Write the label as key=value, for example team=storage.")
            }
            Self::InvalidLabel => f.write_str(
                "Label keys and values use lower-case letters, digits, '-' and '_', at most 63 characters each, and a key starts with a letter, for example team=storage.",
            ),
            Self::Repeated(member) => {
                write!(f, "{member} was already given for this bucket; give it once.")
            }
            Self::PublicUnderPrevention { index } => write!(
                f,
                "buckets[{index}] enforces public access prevention, so its iamPolicy cannot grant allUsers or allAuthenticatedUsers; drop public read and those members, or keep publicAccessPrevention inherited."
            ),
            Self::Rejected(diagnostics) => diagnostics.fmt(f),
            Self::Unquotable => f.write_str(
                "The env format cannot quote the ' in this document; use --format json or compose.",
            ),
            Self::Encode(_) => f.write_str("Cannot encode the buckets document."),
        }
    }
}

impl std::error::Error for GenerateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Rejected(diagnostics) => Some(diagnostics),
            Self::Encode(error) => Some(error),
            Self::InvalidName
            | Self::UnpairedLabel
            | Self::InvalidLabel
            | Self::Repeated(_)
            | Self::PublicUnderPrevention { .. }
            | Self::Unquotable => None,
        }
    }
}

#[cfg(test)]
mod tests;
