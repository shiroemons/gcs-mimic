//! Direct local fixtures share the ordinary streaming and atomic publication protocol.

use crate::{
    auth::AuthFailure,
    json::{self, JsonError},
    notification::NotificationMode,
    object::{ExpectedHashes, ObjectError, UploadChunk},
    server::{DeadlineWatch, RequestBody, RequestCtx, json_response},
    storage::admin::{
        AdminError, BucketName, Generation, ImportedIdentity, ObjectName, Publication, PublishMode,
    },
};
use http_body_util::{BodyExt, Full};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Body, Bytes},
};
use serde_json::Value;
use std::{
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;

struct Fixture {
    bucket: BucketName,
    name: ObjectName,
    publication: Publication,
    identity: ImportedIdentity,
    notification_mode: NotificationMode,
}

pub(super) async fn insert(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let admin = super::authority(&request, ctx)?;
    if request.method() != Method::POST {
        let mut response = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "Use POST /_internal/objects with multipart/related",
        )
        .into_response();
        response
            .headers_mut()
            .insert("allow", hyper::header::HeaderValue::from_static("POST"));
        return Ok(response);
    }
    if request.uri().query().is_some() {
        return Err(JsonError::invalid_argument(
            "Fixture parameters belong in the metadata JSON, not the query",
        ));
    }
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let object = ctx.object(None, deadline);
    let boundary = json::multipart_boundary(request.headers())?;
    let activity = ctx.activity;
    let body = request.into_body().map_frame(|frame| {
        activity.send_modify(|progress| progress.awaiting_catalog = false);
        frame
    });
    let mut multipart = json::Multipart::new(body, &boundary);
    let metadata = multipart.metadata().await?;
    let mut fixture = parse(
        &metadata,
        ctx.state.clock.now(),
        ctx.config.max_object_bytes,
    )?;
    let admin = admin.with_notification_mode(fixture.notification_mode);
    if let Some(content_type) = multipart.headers(json::PartKind::Media).await?
        && fixture.publication.metadata.content_type.is_none()
    {
        fixture.publication.content_type = content_type;
    }
    let expected = fixture.publication.expected;
    let (sender, body) = mpsc::channel(2);
    let upload = admin.publish_with_identity(
        &fixture.bucket,
        &fixture.name,
        fixture.publication,
        Some(fixture.identity),
        body,
    );
    tokio::pin!(upload);
    let streaming = stream(
        &mut multipart,
        &sender,
        expected,
        ctx.config.max_object_bytes,
    );
    let record = tokio::select! {
        result = &mut upload => result.map_err(failure)?,
        result = streaming => {
            let expected = match result {
                Ok(expected) => expected,
                Err(error) => {
                    let _ = sender.send(Err(io::Error::other("invalid fixture body"))).await;
                    drop(sender);
                    match object.await_catalog(upload).await {
                        Ok(_) | Err(AdminError::BodyAborted(_)) => {},
                        Err(error) => return Err(failure(error)),
                    }
                    return Err(error);
                }
            };
            object.await_catalog(async {
                let _ = sender.send(Ok(UploadChunk::EndWithHashes(expected))).await;
                drop(sender);
                upload.await
            }).await.map_err(failure)?
        }
    };
    Ok(json_response(&json::object_resource(
        &record,
        &object.endpoints.api_base_url,
        json::Projection::NoAcl,
    )?))
}

async fn stream<B: Body<Data = Bytes> + Unpin>(
    multipart: &mut json::Multipart<B>,
    sender: &mpsc::Sender<Result<UploadChunk, io::Error>>,
    expected: ExpectedHashes,
    max_bytes: u64,
) -> Result<ExpectedHashes, JsonError> {
    let mut size = 0_u64;
    while let Some(bytes) = multipart.next_chunk().await? {
        size = size
            .checked_add(bytes.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or_else(|| JsonError::from(ObjectError::TooLarge))?;
        if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
            return Ok(expected);
        }
    }
    let expected = multipart.checksums(expected).await?;
    multipart.finish().await?;
    Ok(expected)
}

fn parse(bytes: &[u8], now: SystemTime, max_bytes: u64) -> Result<Fixture, JsonError> {
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|_| JsonError::invalid_argument("Fixture metadata must be a JSON object"))?;
    let fields = value
        .as_object_mut()
        .ok_or_else(|| JsonError::invalid_argument("Fixture metadata must be a JSON object"))?;
    let notification_mode = match fields.remove("notificationMode") {
        None => NotificationMode::Emit,
        Some(Value::String(mode)) if mode == "emit" => NotificationMode::Emit,
        Some(Value::String(mode)) if mode == "suppress" => NotificationMode::Suppress,
        _ => {
            return Err(JsonError::invalid_argument(
                "Fixture notificationMode must be emit or suppress",
            ));
        }
    };
    let bucket = fields
        .get("bucket")
        .and_then(Value::as_str)
        .ok_or_else(|| JsonError::invalid_argument("Fixture metadata requires bucket"))?;
    let bucket = BucketName::new(bucket).map_err(failure)?;
    let generation = fields
        .remove("generation")
        .map(|value| {
            let generation = value
                .as_str()
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| (1..i64::MAX).contains(value))
                .ok_or_else(|| {
                    JsonError::invalid_argument(
                        "Fixture generation must be a positive i64 string below i64::MAX",
                    )
                })?;
            Generation::new(generation).map_err(failure)
        })
        .transpose()?;
    if fields.contains_key("created") && fields.contains_key("timeCreated") {
        return Err(JsonError::invalid_argument(
            "Use either created or timeCreated, not both",
        ));
    }
    let created = fields
        .remove("created")
        .or_else(|| fields.remove("timeCreated"));
    let created = timestamp(created.as_ref(), now)?;
    let updated = timestamp(fields.remove("updated").as_ref(), now)?;
    let identity = ImportedIdentity::fixture(generation, created, updated).map_err(failure)?;
    for field in ["timeDeleted", "deleted", "etag", "size"] {
        if fields.contains_key(field) {
            return Err(JsonError::unsupported_object(field));
        }
    }
    let acl = fields
        .remove("acl")
        .map(|acl| fixture_acl(&acl))
        .transpose()?
        .flatten();
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| JsonError::invalid_argument("Invalid fixture metadata"))?;
    let (name, mut metadata, expected) =
        json::multipart_metadata(&bytes, None, ExpectedHashes::default())?;
    metadata.acl = acl;
    let name = ObjectName::new(name).map_err(failure)?;
    let content_type = metadata
        .content_type
        .as_ref()
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(|| "application/octet-stream".into());
    metadata.validate()?;
    Ok(Fixture {
        bucket,
        name,
        identity,
        notification_mode,
        publication: Publication {
            content_type,
            metadata,
            expected,
            condition: None,
            max_bytes,
            mode: PublishMode::Replace,
        },
    })
}

fn fixture_acl(acl: &Value) -> Result<Option<Vec<crate::acl::AclEntry>>, JsonError> {
    if let Some(entries) = acl.as_array() {
        for entry in entries.iter().filter_map(Value::as_object) {
            for (field, value) in entry {
                if matches!(field.as_str(), "entity" | "role") {
                    continue;
                }
                if matches!(
                    field.as_str(),
                    "entityId" | "domain" | "email" | "projectTeam"
                ) && (value.is_null() || value.as_str() == Some(""))
                {
                    continue;
                }
                return Err(JsonError::unsupported_object(&format!("acl.{field}")));
            }
        }
    }
    let bytes = serde_json::to_vec(&serde_json::json!({"acl": acl}))
        .map_err(|_| JsonError::invalid_argument("Invalid fixture ACL"))?;
    Ok(
        json::parse_object_metadata_mutation(&bytes, json::WriteMode::Patch)?
            .update
            .acl,
    )
}

fn timestamp(value: Option<&Value>, default: SystemTime) -> Result<SystemTime, JsonError> {
    let micros = match value {
        Some(value) => crate::import::codec::parse_timestamp(value.as_str().ok_or_else(|| {
            JsonError::invalid_argument("Fixture timestamps must be RFC3339 strings")
        })?)
        .map_err(JsonError::invalid_argument)?,
        None => {
            let elapsed = default
                .duration_since(UNIX_EPOCH)
                .map_err(|_| JsonError::invalid_argument("Fixture clock precedes Unix epoch"))?;
            i64::try_from(elapsed.as_micros())
                .map_err(|_| JsonError::invalid_argument("Fixture clock is out of range"))?
        }
    };
    let duration = Duration::from_micros(micros.unsigned_abs());
    if micros < 0 {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    }
    .ok_or_else(|| JsonError::invalid_argument("Fixture timestamp is out of range"))
}

fn failure(error: AdminError) -> JsonError {
    let (status, reason, message) = match error {
        AdminError::Precondition => (
            StatusCode::CONFLICT,
            "conflict",
            "Fixture generation already exists; choose a new generation",
        ),
        AdminError::NotFound => (
            StatusCode::NOT_FOUND,
            "notFound",
            "Create the fixture bucket before publishing objects",
        ),
        AdminError::Checksum => (
            StatusCode::BAD_REQUEST,
            "invalid",
            "Fixture checksum does not match the body",
        ),
        AdminError::TooLarge => return ObjectError::TooLarge.into(),
        AdminError::Invalid(message) => return JsonError::invalid_argument(message),
        AdminError::Hold => (
            StatusCode::FORBIDDEN,
            "forbidden",
            "Existing object is protected by a hold or retention",
        ),
        AdminError::Forbidden => return AuthFailure::Forbidden.into(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "backendError",
            "Fixture publication failed; inspect local storage and retry",
        ),
    };
    JsonError::new(status, reason, message)
}
