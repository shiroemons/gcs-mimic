//! Bounded XML creation metadata is converted to the shared validated bucket record.

use super::*;
use crate::xml::document::{self, malformed};
use serde_json::{Value, json};

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
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
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
    let sha = auth::signature::payload_hash(request.headers()).map_err(|_| {
        Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidDigest",
            "Invalid SHA256 declaration",
        )
    })?;
    let md5 = super::super::put::single(request.headers(), "content-md5")?
        .map(json::decode_hash::<16>)
        .transpose()?;
    let bytes = json::read_body(request.into_body(), 64 * 1024).await?;
    if sha.is_some_and(|hash| {
        hash.as_slice() != ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref()
    }) {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "BadDigest",
            "The body SHA256 does not match",
        ));
    }
    if let Some(expected) = md5 {
        let mut hasher = crate::hashing::Hasher::default();
        hasher.update(&bytes);
        if hasher.finish().md5 != expected {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "BadDigest",
                "The body MD5 does not match",
            ));
        }
    }
    let mut value = parse(&bytes, bucket, ctx.config.compat_profile)?;
    if let Some(period) = period {
        value["retentionPolicy"] = json!({"retentionPeriod": period.to_string()});
    }
    let mut record = json::parse_insert_with_profile(
        value.to_string().as_bytes(),
        &project,
        ctx.state.clock.now(),
        ctx.config.compat_profile,
        "US",
    )?;
    record.object_retention_enabled = enabled;
    ctx.state
        .catalog
        .insert_bucket(record)
        .await
        .map_err(|error| catalog_error(error, caller))?;
    Ok(boxed(Response::new(Full::new(Bytes::new()))))
}

fn parse(
    bytes: &[u8],
    bucket: &str,
    profile: crate::config::CompatProfile,
) -> Result<Value, Error> {
    let mut value = json!({"name": bucket});
    if !bytes.is_empty() {
        let root = document::read(bytes)?;
        if root.name != "CreateBucketConfiguration" || !root.text.trim().is_empty() {
            return Err(malformed());
        }
        for child in root.children {
            let field = match child.name.as_str() {
                "LocationConstraint" => "location",
                "StorageClass" => "storageClass",
                "CustomPlacementConfig" | "EncryptionConfiguration" => {
                    return Err(Error::unsupported(&child.name));
                }
                _ => return Err(malformed()),
            };
            if value.get(field).is_some() || !child.children.is_empty() {
                return Err(malformed());
            }
            value[field] = Value::String(child.text.trim().into());
        }
    }
    if profile == crate::config::CompatProfile::LegacyEmulator
        && (value.get("location").is_some() || value.get("storageClass").is_some())
    {
        return Err(Error::unsupported(
            "legacy bucket creation settings; use the native compatibility profile",
        ));
    }
    Ok(value)
}
