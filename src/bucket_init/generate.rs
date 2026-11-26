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
use std::{collections::BTreeMap, str::FromStr};

mod summary;

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

crate::config::string_enum! {
    no_default
    /// The authentication mode the written settings start the server in, which decides who reads
    /// a bucket without public read: anyone in `permissive`, the server's default, and only the
    /// application's servers and the URLs they sign in `strict`. No member of a bucket says
    /// this, so the document carries it for the settings written beside the buckets.
    pub enum AuthMode {
        Permissive => "permissive",
        Strict => "strict",
    }
    "auth mode must be permissive or strict"
}

/// The `serve` options that start the server as the strict settings below do, for a buckets file,
/// which holds only buckets.
pub const STRICT_SERVE_OPTIONS: &str =
    "--auth-mode strict --trusted-listen 127.0.0.1:4444 --test-signer enabled";

/// Strict mode's settings as `.env` lines for `gcs-mimic serve` on the machine that runs the
/// application's servers. The application's servers send no credentials, as browsers do not, so
/// they get a trusted listener of their own, on loopback so that no other machine reaches it. The
/// built-in test signer signs the URLs that browsers use for private objects. No comment lines:
/// zsh runs a pasted `#` line as a command unless `interactive_comments` is set.
const STRICT_ENV: &str = "GCS_MIMIC_AUTH_MODE=strict
GCS_MIMIC_TRUSTED_LISTEN=127.0.0.1:4444
GCS_MIMIC_TEST_SIGNER=enabled
";

/// Strict mode's settings for a Compose service, as in the strict local recipe. In a container
/// the trusted listener binds every address so that the application's containers reach it, and
/// leaving its port out of `ports:` is what keeps everything else out, so the block says so.
const STRICT_COMPOSE: &str = "  GCS_MIMIC_AUTH_MODE: strict
  # Your servers on the Compose network use port 4444 without credentials. Never publish it:
  # anything that reaches it acts as your app.
  GCS_MIMIC_TRUSTED_LISTEN: 0.0.0.0:4444
  # Signs URLs for browsers with a key that ships with gcs-mimic, so use it only locally.
  GCS_MIMIC_TEST_SIGNER: enabled
";

/// The role that public read grants to `allUsers`.
const PUBLIC_READ_ROLE: &str = "roles/storage.objectViewer";

/// The longest soft delete retention startup accepts, so that a question can refuse a longer one
/// as it is typed.
pub const MAX_SOFT_DELETE_RETENTION_SECONDS: u64 = crate::bucket::MAX_SOFT_DELETE_RETENTION_SECONDS;

/// The longest CORS max age startup accepts, since a bucket stores it as an `i32`.
pub const MAX_CORS_MAX_AGE_SECONDS: u32 = i32::MAX.unsigned_abs();

/// The methods browser uploads use: `PUT` sends a file, and `POST` starts a resumable upload.
pub const UPLOAD_METHODS: [&str; 2] = ["PUT", "POST"];
/// The request headers browser uploads send, which a preflight refuses unless the rule lists
/// them: `Content-Type` with a file, and `x-goog-resumable` to start a resumable upload.
pub const UPLOAD_HEADERS: [&str; 2] = ["Content-Type", "x-goog-resumable"];
/// The methods browser downloads use: `GET` reads an object and `HEAD` its metadata.
pub const DOWNLOAD_METHODS: [&str; 2] = ["GET", "HEAD"];

/// The storage classes a prompt offers. The legacy `MULTI_REGIONAL`, `REGIONAL` and
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

/// A CORS origin that startup accepts, checked when it is read as a label is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsOrigin(String);

impl FromStr for CorsOrigin {
    type Err = GenerateError;

    fn from_str(origin: &str) -> Result<Self, Self::Err> {
        if !crate::bucket::valid_cors_origin(origin) {
            return Err(GenerateError::InvalidCorsOrigin);
        }
        Ok(Self(origin.to_owned()))
    }
}

impl std::fmt::Display for CorsOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A CORS method that startup accepts: an HTTP method or `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsMethod(String);

impl FromStr for CorsMethod {
    type Err = GenerateError;

    fn from_str(method: &str) -> Result<Self, Self::Err> {
        if !crate::bucket::valid_cors_method(method) {
            return Err(GenerateError::InvalidCorsMethod);
        }
        Ok(Self(method.to_owned()))
    }
}

/// A CORS response header that startup accepts: a header name or `*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorsResponseHeader(String);

impl FromStr for CorsResponseHeader {
    type Err = GenerateError;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        if !crate::bucket::valid_cors_response_header(name) {
            return Err(GenerateError::InvalidCorsResponseHeader);
        }
        Ok(Self(name.to_owned()))
    }
}

/// The members of one IAM binding, written comma-separated as `--iam-binding` and the question
/// take them; spaces around each member are dropped. Each is checked by the rules startup applies
/// to a policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Members(Vec<String>);

/// Members chosen one at a time, such as from a list, form one binding.
impl FromIterator<Members> for Members {
    fn from_iter<I: IntoIterator<Item = Members>>(lists: I) -> Self {
        Self(lists.into_iter().flat_map(|Members(list)| list).collect())
    }
}

impl FromStr for Members {
    type Err = GenerateError;

    fn from_str(list: &str) -> Result<Self, Self::Err> {
        use crate::auth::policy_input::{PolicyInputError, parse_member};
        list.split(',')
            .map(str::trim)
            .map(|member| match parse_member(member) {
                Ok(_) => Ok(member.to_owned()),
                Err(PolicyInputError::UnsupportedInput(_)) => Err(GenerateError::UnsupportedMember),
                Err(PolicyInputError::InvalidInput(_)) => Err(GenerateError::InvalidMember),
            })
            .collect::<Result<_, _>>()
            .map(Self)
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
    CorsOrigin(CorsOrigin),
    CorsMethod(CorsMethod),
    CorsResponseHeader(CorsResponseHeader),
    CorsMaxAgeSeconds(u64),
    /// Adds [`UPLOAD_METHODS`] and [`UPLOAD_HEADERS`] to the CORS rule, whose origins come from
    /// [`Setting::CorsOrigin`].
    BrowserUploads,
    /// Adds [`DOWNLOAD_METHODS`] to the CORS rule.
    BrowserDownloads,
    /// Grants `allUsers` object reads through `iamPolicy`.
    PublicRead,
    IamBinding {
        role: String,
        members: Members,
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
            Self::BrowserUploads => "browser uploads",
            Self::BrowserDownloads => "browser downloads",
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

impl Cors {
    /// What the rule lacks before any request can match it: a request matches a rule only
    /// through one of its origins and one of its methods. Startup accepts a rule that matches
    /// nothing, so a helper that wrote one would report success for CORS that never applies.
    fn gap(&self) -> Option<CorsGap> {
        match (self.origin.is_empty(), self.method.is_empty()) {
            (false, false) => None,
            (true, false) => Some(CorsGap::Origin),
            (false, true) => Some(CorsGap::Method),
            (true, true) => Some(CorsGap::OriginAndMethod),
        }
    }

    /// Whether a browser upload that sets `Content-Type` fails its preflight under this rule:
    /// the rule lets browsers upload, but the preflight's requested headers must each be one of
    /// its response headers.
    fn refuses_upload_content_type(&self) -> bool {
        let uploads = self
            .method
            .iter()
            .any(|method| matches!(method.as_str(), "PUT" | "POST" | "*"));
        uploads
            && !self
                .response_header
                .iter()
                .any(|header| header == "*" || header.eq_ignore_ascii_case("Content-Type"))
    }
}

/// What a bucket's CORS rule lacks before it can match a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorsGap {
    Origin,
    Method,
    OriginAndMethod,
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

    /// The same answers for a bucket named `name`, so that several buckets can share them.
    pub fn with_name(&self, name: &str) -> Result<Self, GenerateError> {
        Ok(Self {
            name: Self::new(name)?.name,
            ..self.clone()
        })
    }

    /// Whether the entry is its name alone, which the inline setting can say without JSON.
    fn is_name_only(&self) -> bool {
        let Self {
            name: _,
            project,
            location,
            storage_class,
            labels,
            cors,
            versioning,
            default_event_based_hold,
            iam_configuration,
            soft_delete_policy,
            iam_policy,
            public_read,
        } = self;
        project.is_none()
            && location.is_none()
            && storage_class.is_none()
            && labels.is_empty()
            && cors.is_none()
            && versioning.is_none()
            && !default_event_based_hold
            && iam_configuration.is_none()
            && soft_delete_policy.is_none()
            && iam_policy.is_none()
            && !public_read
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
            Setting::Label(label) => self.add_label(label)?,
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
            Setting::CorsOrigin(CorsOrigin(origin)) => push(&mut self.cors_rule().origin, origin),
            Setting::CorsMethod(CorsMethod(method)) => push(&mut self.cors_rule().method, method),
            Setting::CorsResponseHeader(CorsResponseHeader(header)) => {
                push(&mut self.cors_rule().response_header, header)
            }
            Setting::CorsMaxAgeSeconds(seconds) => {
                set_once(&mut self.cors_rule().max_age_seconds, seconds)
            }
            Setting::BrowserUploads => {
                let rule = self.cors_rule();
                extend(&mut rule.method, &UPLOAD_METHODS);
                extend(&mut rule.response_header, &UPLOAD_HEADERS);
                true
            }
            Setting::BrowserDownloads => {
                extend(&mut self.cors_rule().method, &DOWNLOAD_METHODS);
                true
            }
            Setting::PublicRead => {
                let fresh = !std::mem::replace(&mut self.public_read, true);
                if fresh {
                    self.bind(PUBLIC_READ_ROLE.to_owned(), vec!["allUsers".to_owned()]);
                }
                fresh
            }
            Setting::IamBinding {
                role,
                members: Members(members),
            } => {
                self.bind(role, members);
                true
            }
        };
        if fresh { Ok(()) } else { Err(repeated) }
    }

    /// Adds a label and returns whether its key is new. A new key past the limit is refused here,
    /// so that a question can ask again instead of the parser refusing the whole document.
    fn add_label(&mut self, Label { key, value }: Label) -> Result<bool, GenerateError> {
        if self.labels.contains_key(&key) {
            return Ok(false);
        }
        if self.labels.len() >= crate::bucket::MAX_LABELS {
            return Err(GenerateError::TooManyLabels);
        }
        self.labels.insert(key, value);
        Ok(true)
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

/// Adds a list value unless the list has it. A list member accepts any number of values, and a
/// goal such as browser uploads may add one that a flag already gave.
fn push(list: &mut Vec<String>, value: String) -> bool {
    if !list.contains(&value) {
        list.push(value);
    }
    true
}

fn extend(list: &mut Vec<String>, values: &[&str]) {
    for value in values {
        push(list, (*value).to_owned());
    }
}

/// A complete buckets document, and the mode the settings written beside it start the server in.
#[derive(Debug, Serialize)]
pub struct Document {
    version: u8,
    buckets: Vec<Bucket>,
    #[serde(skip)]
    auth_mode: AuthMode,
}

impl Document {
    /// The document for the server's default permissive mode, which needs no settings beside it.
    pub fn new(buckets: Vec<Bucket>) -> Self {
        Self {
            version: 1,
            buckets,
            auth_mode: AuthMode::Permissive,
        }
    }

    /// The same buckets with settings beside them, where the format has room for settings, that
    /// start the server in `auth_mode`.
    #[must_use]
    pub fn with_auth_mode(self, auth_mode: AuthMode) -> Self {
        Self { auth_mode, ..self }
    }

    pub fn auth_mode(&self) -> AuthMode {
        self.auth_mode
    }

    /// The document as `format` writes it, after the text the server will read passes the
    /// startup parser with the default `seed_project` and `default_location`. Checks that depend on
    /// the catalog, such as a project's public access prevention, still happen at startup. In
    /// strict mode `env` and `compose` write that mode's settings before the buckets; a buckets
    /// file holds only buckets, so `json` leaves them to [`STRICT_SERVE_OPTIONS`].
    pub fn render(&self, format: Format) -> Result<String, GenerateError> {
        if let Some((bucket, gap)) = self.buckets.iter().find_map(|bucket| {
            let gap = bucket.cors.as_ref()?.gap()?;
            Some((bucket.name.clone(), gap))
        }) {
            return Err(GenerateError::IncompleteCors { bucket, gap });
        }
        if let Some(index) = self
            .buckets
            .iter()
            .position(Bucket::grants_public_access_under_prevention)
        {
            return Err(GenerateError::PublicUnderPrevention { index });
        }
        let text = match format {
            Format::Env => serde_json::to_string(self).map_err(GenerateError::Encode)?,
            Format::Json => serde_json::to_string_pretty(self).map_err(GenerateError::Encode)?,
            Format::Compose => self.compose_json()?,
        };
        let defaults = Config::default();
        let form = match format {
            Format::Json => Form::File,
            Format::Env | Format::Compose => Form::Inline,
        };
        parse(
            text.as_bytes(),
            form,
            defaults.seed_project(),
            &SystemClock,
            defaults.default_location(),
            None,
        )
        .map_err(GenerateError::Rejected)?;
        let settings = match (format, self.auth_mode) {
            (_, AuthMode::Permissive) | (Format::Json, AuthMode::Strict) => "",
            (Format::Env, AuthMode::Strict) => STRICT_ENV,
            (Format::Compose, AuthMode::Strict) => STRICT_COMPOSE,
        };
        match (format, self.shorthand()) {
            (Format::Json, _) => Ok(format!("{text}\n")),
            (Format::Env, Some(names)) => Ok(format!("{settings}GCS_MIMIC_BUCKETS={names}\n")),
            (Format::Env, None) => Ok(format!("{settings}{}", env_line(&text)?)),
            // Quoted, since YAML would read a bucket named null or true as no string at all.
            (Format::Compose, Some(names)) => Ok(format!(
                "environment:\n{settings}  GCS_MIMIC_BUCKETS: \"{names}\"\n"
            )),
            (Format::Compose, None) => {
                Ok(format!("environment:\n{settings}{}", compose_member(&text)))
            }
        }
    }

    /// The bucket names, comma-separated, when the names alone say everything. The inline
    /// setting reads them as the document with one name-only entry each, which is this document,
    /// so the text the parser checked is the text startup reads. A bucket file takes only a
    /// document. Names hold no comma, quote, `$` or space, so they need no quoting.
    fn shorthand(&self) -> Option<String> {
        self.buckets.iter().all(Bucket::is_name_only).then(|| {
            let names: Vec<_> = self.buckets.iter().map(Bucket::name).collect();
            names.join(",")
        })
    }

    /// The document for a Compose block with one bucket on each line, so that a block of several
    /// buckets stays short enough to read and edit there.
    fn compose_json(&self) -> Result<String, GenerateError> {
        let entries = self
            .buckets
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .map_err(GenerateError::Encode)?;
        Ok(format!(
            "{{\"version\": {}, \"buckets\": [\n  {}\n]}}",
            self.version,
            entries.join(",\n  ")
        ))
    }

    /// The buckets whose CORS rule lets browsers upload but refuses the preflight of an upload
    /// that sets `Content-Type`, as a browser does for a file. Startup accepts such a rule, since
    /// an upload without that header passes, so this is advice rather than a refusal.
    pub fn uploads_refusing_content_type(&self) -> impl Iterator<Item = &str> {
        self.buckets
            .iter()
            .filter(|bucket| {
                bucket
                    .cors
                    .as_ref()
                    .is_some_and(Cors::refuses_upload_content_type)
            })
            .map(Bucket::name)
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

/// The `GCS_MIMIC_BUCKETS` member of an `environment:` block to paste into a service. Compose
/// interpolates `$` in values, and `$$` is its literal dollar.
fn compose_member(text: &str) -> String {
    let mut snippet = String::from("  GCS_MIMIC_BUCKETS: |\n");
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
    /// A new label key would pass the most labels a bucket can have.
    TooManyLabels,
    InvalidCorsOrigin,
    InvalidCorsMethod,
    InvalidCorsResponseHeader,
    /// A member is not a principal a policy can name.
    InvalidMember,
    /// A member names a principal kind that the emulator does not model, such as a group.
    UnsupportedMember,
    /// A single-valued member was given twice for one bucket.
    Repeated(&'static str),
    /// The bucket's CORS rule lacks what any request needs to match it.
    IncompleteCors {
        bucket: String,
        gap: CorsGap,
    },
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
            Self::TooManyLabels => write!(
                f,
                "A bucket can have at most {} labels.",
                crate::bucket::MAX_LABELS
            ),
            Self::InvalidCorsOrigin => f.write_str(
                "A CORS origin is visible ASCII without spaces, for example http://localhost:3000 or *.",
            ),
            Self::InvalidCorsMethod => {
                f.write_str("A CORS method is an HTTP method, for example GET, or *.")
            }
            Self::InvalidCorsResponseHeader => f.write_str(
                "A CORS response header is a header name, for example Content-Type, or *.",
            ),
            Self::InvalidMember => f.write_str(super::policy::MEMBER_FIX),
            Self::UnsupportedMember => f.write_str(super::policy::UNSUPPORTED_MEMBER_FIX),
            Self::Repeated(member) => {
                write!(f, "{member} was already given for this bucket; give it once.")
            }
            Self::IncompleteCors { bucket, gap } => {
                let lacks = match gap {
                    CorsGap::Origin => "an origin",
                    CorsGap::Method => "a method",
                    CorsGap::OriginAndMethod => "an origin or a method",
                };
                write!(
                    f,
                    "The CORS rule of {bucket} has no {lacks}, so it matches no request; give at least one CORS origin and one CORS method."
                )
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
            | Self::TooManyLabels
            | Self::InvalidCorsOrigin
            | Self::InvalidCorsMethod
            | Self::InvalidCorsResponseHeader
            | Self::InvalidMember
            | Self::UnsupportedMember
            | Self::Repeated(_)
            | Self::IncompleteCors { .. }
            | Self::PublicUnderPrevention { .. }
            | Self::Unquotable => None,
        }
    }
}

#[cfg(test)]
mod tests;
