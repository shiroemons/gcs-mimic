//! Transport-independent bucket state shared by the catalog and protocol adapters.

use std::{collections::BTreeMap, fmt, time::SystemTime};

/// Bucket metadata persisted by the catalog. Protocol adapters derive wire fields from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketRecord {
    pub name: String,
    /// Project identifier exactly as supplied by the creating request.
    pub project: String,
    pub location: String,
    pub location_type: String,
    pub storage_class: String,
    pub labels: BTreeMap<String, String>,
    pub cors: Vec<CorsRule>,
    pub versioning_enabled: bool,
    pub default_event_based_hold: bool,
    pub public_access_prevention_enforced: bool,
    pub metageneration: i64,
    /// Microsecond precision; the catalog floors sub-microsecond values.
    pub time_created: SystemTime,
    pub updated: SystemTime,
}

#[derive(Clone, Debug, Default)]
pub struct BucketMetadataUpdate {
    pub cors: Option<Vec<CorsRule>>,
    pub labels: Option<Option<BTreeMap<String, Option<String>>>>,
    pub storage_class: Option<String>,
    pub versioning_enabled: Option<bool>,
    pub default_event_based_hold: Option<bool>,
    pub public_access_prevention_enforced: Option<bool>,
    pub location: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CorsRule {
    pub origin: Vec<String>,
    pub method: Vec<String>,
    pub response_header: Vec<String>,
    pub max_age_seconds: Option<i32>,
}

pub(crate) fn cors_from_value(value: &serde_json::Value) -> Result<Vec<CorsRule>, CatalogError> {
    let invalid = || CatalogError::InvalidMetadata("Invalid bucket CORS configuration".into());
    let values = value.as_array().ok_or_else(invalid)?;
    let rules = values
        .iter()
        .map(|value| {
            let value = value.as_object().ok_or_else(invalid)?;
            let strings = |field| -> Result<Vec<String>, CatalogError> {
                match value.get(field) {
                    None => Ok(Vec::new()),
                    Some(value) => value
                        .as_array()
                        .ok_or_else(invalid)?
                        .iter()
                        .map(|value| value.as_str().map(str::to_owned).ok_or_else(invalid))
                        .collect(),
                }
            };
            Ok(CorsRule {
                origin: strings("origin")?,
                method: strings("method")?,
                response_header: strings("responseHeader")?,
                max_age_seconds: value
                    .get("maxAgeSeconds")
                    .map(|value| {
                        value
                            .as_i64()
                            .and_then(|value| i32::try_from(value).ok())
                            .ok_or_else(invalid)
                    })
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, CatalogError>>()?;
    validate_cors(&rules)?;
    Ok(rules)
}

pub(crate) fn cors_value(rules: &[CorsRule]) -> serde_json::Value {
    serde_json::Value::Array(
        rules
            .iter()
            .map(|rule| {
                let mut value = serde_json::Map::new();
                for (field, strings) in [
                    ("origin", &rule.origin),
                    ("method", &rule.method),
                    ("responseHeader", &rule.response_header),
                ] {
                    if !strings.is_empty() {
                        value.insert(field.into(), serde_json::json!(strings));
                    }
                }
                if let Some(seconds) = rule.max_age_seconds {
                    value.insert("maxAgeSeconds".into(), seconds.into());
                }
                value.into()
            })
            .collect(),
    )
}

pub(crate) fn validate_cors(rules: &[CorsRule]) -> Result<(), CatalogError> {
    let mut bytes = 2usize;
    for rule in rules {
        bytes = bytes.saturating_add(3);
        for value in rule
            .origin
            .iter()
            .chain(&rule.method)
            .chain(&rule.response_header)
        {
            bytes = bytes.saturating_add(value.len()).saturating_add(3);
        }
        if rule.max_age_seconds.is_some() {
            bytes = bytes.saturating_add(18);
        }
        if bytes > 64 * 1024 {
            return Err(CatalogError::InvalidMetadata(
                "Bucket CORS exceeds 64 KiB".into(),
            ));
        }
        if rule.origin.iter().any(|origin| {
            origin.is_empty()
                || !origin.is_ascii()
                || origin
                    .bytes()
                    .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
                || hyper::header::HeaderValue::from_str(origin).is_err()
        }) || rule
            .method
            .iter()
            .any(|method| method != "*" && hyper::Method::from_bytes(method.as_bytes()).is_err())
            || rule.response_header.iter().any(|name| {
                name != "*" && hyper::header::HeaderName::from_bytes(name.as_bytes()).is_err()
            })
            || rule.max_age_seconds.is_some_and(|seconds| seconds < 0)
        {
            return Err(CatalogError::InvalidMetadata(
                "Invalid bucket CORS configuration".into(),
            ));
        }
    }
    if cors_value(rules).to_string().len() > 64 * 1024 {
        return Err(CatalogError::InvalidMetadata(
            "Bucket CORS exceeds 64 KiB".into(),
        ));
    }
    Ok(())
}

pub(crate) fn valid_label(value: &str, key: bool) -> bool {
    let initial = |character: char| {
        character.is_ascii_lowercase()
            || (!character.is_ascii() && character.is_alphabetic() && !character.is_uppercase())
    };
    value.chars().count() <= 63
        && (!key || value.chars().next().is_some_and(initial))
        && value.chars().all(|character| {
            initial(character) || character.is_numeric() || matches!(character, '_' | '-')
        })
}

pub(crate) fn validate_storage_class(storage_class: &str) -> Result<(), CatalogError> {
    match storage_class {
        "STANDARD"
        | "NEARLINE"
        | "COLDLINE"
        | "ARCHIVE"
        | "MULTI_REGIONAL"
        | "DURABLE_REDUCED_AVAILABILITY" => Ok(()),
        "REGIONAL" | "RAPID" => Err(CatalogError::UnsupportedMetadata(
            "storageClass requires a region or zone".into(),
        )),
        _ => Err(CatalogError::InvalidMetadata(
            "Invalid bucket storageClass".into(),
        )),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CatalogError {
    AlreadyExists,
    NotFound,
    NotEmpty,
    PreconditionFailed,
    PublicAccessPrevention,
    /// The bucket's current owning project is not one the caller was authorized for.
    Forbidden,
    InvalidMetadata(String),
    UnsupportedMetadata(String),
    Unavailable(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("bucket already exists"),
            Self::NotFound => formatter.write_str("bucket not found"),
            Self::NotEmpty => formatter.write_str("bucket is not empty"),
            Self::PreconditionFailed => formatter.write_str("precondition failed"),
            Self::PublicAccessPrevention => {
                formatter.write_str("public access prevention is enforced")
            }
            Self::Forbidden => formatter.write_str("forbidden"),
            Self::InvalidMetadata(reason) | Self::UnsupportedMetadata(reason) => {
                formatter.write_str(reason)
            }
            Self::Unavailable(reason) => write!(formatter, "catalog unavailable: {reason}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// The adapter's bucket authorization, re-validated by the catalog inside the same transaction
/// that reads or mutates the bucket.
///
/// Authorization resolves the owning project in an earlier catalog call. The bucket can be
/// deleted and recreated under another project before the operation runs, so the operation
/// itself must confirm that the project it observes is one the caller was authorized for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BucketAccess {
    /// Permissive profile: no authorization applies and overwrites are allowed.
    #[default]
    Unchecked,
    /// Re-evaluate effective grants against the policy selected by the operation transaction.
    Policy {
        identity: Option<crate::auth::Identity>,
        permission: String,
        project: Option<String>,
    },
}
