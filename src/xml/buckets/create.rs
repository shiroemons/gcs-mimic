//! Bounded XML creation metadata is converted to the shared validated bucket record.

use super::*;
use crate::xml::document::{self, malformed};
use serde_json::{Map, Value};

pub(super) async fn respond(
    request: Request<RequestBody>,
    bucket: &str,
    ctx: RequestCtx<'_>,
    caller: Option<auth::Caller<'_>>,
) -> Result<Response<ResponseBody>, Error> {
    let project = project(request.headers())?.to_owned();
    authorize_project(caller, auth::BUCKETS_CREATE, &project)?;
    let retention =
        super::super::put::single(request.headers(), "x-goog-bucket-object-lock-enabled")?;
    let enabled = match retention {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => {
            return Err(Error::invalid(
                "x-goog-bucket-object-lock-enabled must be true or false",
            ));
        }
    };
    if enabled {
        authorize_project(caller, auth::BUCKETS_ENABLE_OBJECT_RETENTION, &project)?;
    }
    validate_query(request.uri(), bucket)?;
    let mut headers = request.headers().clone();
    for name in [
        "x-goog-project-id",
        "x-goog-bucket-object-lock-enabled",
        "x-goog-bucket-retention-period",
        "content-length",
        "transfer-encoding",
        "x-goog-content-sha256",
        "x-amz-content-sha256",
    ] {
        headers.remove(name);
    }
    validate_headers(&headers)?;
    if request.headers().contains_key("content-encoding") {
        return Err(Error::unsupported("content-encoding on bucket creation"));
    }
    let period = super::super::put::single(request.headers(), "x-goog-bucket-retention-period")?
        .map(|raw| super::super::number(raw, super::super::MinValue::One))
        .transpose()?;
    let sha = auth::signature::PayloadHash::of(request.headers())
        .map(auth::signature::PayloadHash::digest)
        .map_err(|_| Error::new(XmlCode::InvalidDigest, "Invalid SHA256 declaration"))?;
    let md5 = super::super::put::single(request.headers(), "content-md5")?
        .map(json::decode_hash::<16>)
        .transpose()?;
    let bytes = json::read_body(request.into_body(), json::RESOURCE_BODY_LIMIT).await?;
    if super::super::sha256_mismatch(sha, &bytes) {
        return Err(Error::new(
            XmlCode::BadDigest,
            "The body SHA256 does not match",
        ));
    }
    if let Some(expected) = md5 {
        let mut hasher = crate::hashing::Hasher::default();
        hasher.update(&bytes);
        if hasher.finish().md5 != expected {
            return Err(Error::new(
                XmlCode::BadDigest,
                "The body MD5 does not match",
            ));
        }
    }
    let configuration = BucketConfiguration::parse(&bytes)?;
    if ctx.config.compat_profile == crate::config::CompatProfile::FakeGcsServer
        && (configuration.location.is_some() || configuration.storage_class.is_some())
    {
        return Err(Error::unsupported(
            "legacy bucket creation settings; use the native compatibility profile",
        ));
    }
    let mut record = json::insert_from_object(
        &configuration.into_insert_body(bucket, period),
        &project,
        ctx.state.clock.now(),
        ctx.config.compat_profile,
        &ctx.state.default_location,
    )?;
    record.object_retention_enabled = enabled;
    ctx.state
        .catalog
        .insert_bucket(record)
        .await
        .map_err(|error| catalog_error(error, caller))?;
    Ok(boxed(empty_response(StatusCode::OK)))
}

/// The settings a `CreateBucketConfiguration` body may carry.
#[derive(Default)]
struct BucketConfiguration {
    location: Option<LocationConstraint>,
    /// `StorageClass` as sent; the shared bucket validation checks it.
    storage_class: Option<String>,
}

/// A `LocationConstraint` element.
enum LocationConstraint {
    /// Empty: the configured default location. Only the JSON API has provider evidence that
    /// an empty location is rejected.
    Default,
    Named(String),
}

impl BucketConfiguration {
    /// An empty body sets nothing. A repeated, nested or unknown element is malformed, and the
    /// placement and encryption settings are unsupported.
    fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let mut configuration = Self::default();
        if bytes.is_empty() {
            return Ok(configuration);
        }
        let root = document::read(bytes)?;
        if root.name != "CreateBucketConfiguration" || !root.text.trim().is_empty() {
            return Err(malformed());
        }
        for child in root.children {
            let text = child.text.trim();
            match child.name.as_str() {
                "LocationConstraint" => {
                    if configuration.location.is_some() || !child.children.is_empty() {
                        return Err(malformed());
                    }
                    configuration.location = Some(if text.is_empty() {
                        LocationConstraint::Default
                    } else {
                        LocationConstraint::Named(text.to_owned())
                    });
                }
                "StorageClass" => {
                    if configuration.storage_class.is_some() || !child.children.is_empty() {
                        return Err(malformed());
                    }
                    configuration.storage_class = Some(text.to_owned());
                }
                "CustomPlacementConfig" | "EncryptionConfiguration" => {
                    return Err(Error::unsupported(&child.name));
                }
                _ => return Err(malformed()),
            }
        }
        Ok(configuration)
    }

    /// The JSON insert members the shared bucket validation reads: the name, the settings that
    /// were sent, with a default location as `null`, and the retention period header.
    fn into_insert_body(self, bucket: &str, retention_period: Option<i64>) -> Map<String, Value> {
        let mut body = Map::from_iter([("name".to_owned(), Value::from(bucket))]);
        if let Some(location) = self.location {
            let location = match location {
                LocationConstraint::Default => Value::Null,
                LocationConstraint::Named(name) => Value::String(name),
            };
            body.insert("location".to_owned(), location);
        }
        if let Some(class) = self.storage_class {
            body.insert("storageClass".to_owned(), Value::String(class));
        }
        if let Some(period) = retention_period {
            let policy = Map::from_iter([(
                "retentionPeriod".to_owned(),
                Value::String(period.to_string()),
            )]);
            body.insert("retentionPolicy".to_owned(), Value::Object(policy));
        }
        body
    }
}
