//! Direct local fixtures share the ordinary streaming and atomic publication protocol.

use crate::config::Config;
use crate::json::JsonErrorResponse;
use crate::server::upload_drive::{
    CatalogWait, DriveError, UploadEnd, UploadSender, drive_upload, upload_channel,
};
use crate::{
    auth::AuthFailure,
    json,
    json::ApiError,
    notification::NotificationMode,
    object::{ExpectedHashes, ObjectError, Patch, UploadChunk, content_type},
    server::{DeadlineWatch, RequestBody, RequestCtx, json_response},
    storage::admin::{
        AdminError, BucketName, Generation, ImportedIdentity, ObjectName, Publication, PublishMode,
    },
};
use http_body_util::Full;
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Body, Bytes},
};
use serde_json::{Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct Fixture {
    bucket: BucketName,
    name: ObjectName,
    publication: Publication,
    provenance: Provenance,
    notification_mode: NotificationMode,
}

/// Which publication a fixture's omitted fields imitate, chosen by its `defaults` member.
enum Provenance {
    /// A direct fixture names its own identity: the catalog keeps the given generation and times
    /// and assigns no owner, and the media part's type stands in for a missing `contentType`.
    Fixture(ImportedIdentity),
    /// A file of the startup seed tree: no imported identity, so ownership, times, generation and
    /// bucket default holds come from the same publication a startup seed runs, and the type
    /// comes from the object name as the seed derives it. A helper that loads a directory at
    /// runtime then creates the objects a startup seed of that directory would.
    Seed,
}

impl Provenance {
    fn into_identity(self) -> Option<ImportedIdentity> {
        match self {
            Self::Fixture(identity) => Some(identity),
            Self::Seed => None,
        }
    }
}

pub(super) async fn insert(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
    authorized: super::Authorized,
) -> Result<Response<Full<Bytes>>, ApiError> {
    let admin = authorized.into_admin();
    if request.method() != Method::POST {
        return Ok(ApiError::method_not_allowed(
            "Use POST /_internal/objects with multipart/related",
            Some("POST"),
        )
        .into_response());
    }
    if request.uri().query().is_some() {
        return Err(ApiError::invalid_argument(
            "Fixture parameters belong in the metadata JSON, not the query",
        ));
    }
    let deadline = ctx.start_object_deadline(DeadlineWatch::Enforced);
    let object = ctx.object(None, deadline);
    let boundary = crate::multipart::multipart_boundary(request.headers())?;
    let mut multipart = json::RelatedUpload::new(request.into_body(), &boundary);
    let metadata = multipart.metadata().await?;
    let mut fixture = parse(&metadata, ctx.state.clock.now(), ctx.config)?;
    let admin = admin.with_notification_mode(fixture.notification_mode);
    if let Some(content_type) = multipart.headers(json::PartKind::Media).await?
        && fixture.publication.metadata.content_type == Patch::Keep
        && matches!(fixture.provenance, Provenance::Fixture(_))
    {
        fixture.publication.content_type = content_type;
    }
    let expected = fixture.publication.expected;
    let (sender, body) = upload_channel();
    let upload = admin.publish_with_identity(
        &fixture.bucket,
        &fixture.name,
        fixture.publication,
        fixture.provenance.into_identity(),
        body,
    );
    let streaming = async {
        stream(
            &mut multipart,
            &sender,
            expected,
            ctx.config.max_object_bytes(),
        )
        .await
        .map(UploadEnd::EofWithHashes)
    };
    let record = drive_upload(CatalogWait::Upload(object), upload, &sender, streaming)
        .await
        .map_err(|error| match error {
            DriveError::Upload(error) => failure(error),
            DriveError::Producer(error) => error,
        })?;
    Ok(json_response(&json::object_resource(
        &record,
        object.endpoints.api_base_url(),
        json::Projection::NoAcl,
    )?))
}

async fn stream<B: Body<Data = Bytes> + Unpin>(
    multipart: &mut json::RelatedUpload<B>,
    sender: &UploadSender,
    expected: ExpectedHashes,
    max_bytes: u64,
) -> Result<ExpectedHashes, ApiError> {
    let mut size = 0_u64;
    while let Some(bytes) = multipart.next_chunk().await? {
        size = size
            .checked_add(bytes.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or_else(|| ApiError::from(ObjectError::TooLarge))?;
        if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
            return Ok(expected);
        }
    }
    let expected = multipart.checksums(expected).await?;
    multipart.finish().await?;
    Ok(expected)
}

fn parse(bytes: &[u8], now: SystemTime, config: &Config) -> Result<Fixture, ApiError> {
    let mut value: Value = serde_json::from_slice(bytes)
        .map_err(|_| ApiError::invalid_argument("Fixture metadata must be a JSON object"))?;
    let fields = value
        .as_object_mut()
        .ok_or_else(|| ApiError::invalid_argument("Fixture metadata must be a JSON object"))?;
    let notification_mode = match fields.remove("notificationMode") {
        None => NotificationMode::Emit,
        Some(Value::String(mode)) if mode == "emit" => NotificationMode::Emit,
        Some(Value::String(mode)) if mode == "suppress" => NotificationMode::Suppress,
        _ => {
            return Err(ApiError::invalid_argument(
                "Fixture notificationMode must be emit or suppress",
            ));
        }
    };
    let bucket = fields
        .get("bucket")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid_argument("Fixture metadata requires bucket"))?;
    let bucket = BucketName::new(bucket).map_err(failure)?;
    let provenance = provenance(fields, now)?;
    for field in ["timeDeleted", "deleted", "etag", "size"] {
        if fields.contains_key(field) {
            return Err(ApiError::unsupported_object(field));
        }
    }
    let acl = fields
        .remove("acl")
        .map(|acl| fixture_acl(&acl))
        .transpose()?
        .unwrap_or_default();
    check_fixture_members(fields)?;
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| ApiError::invalid_argument("Invalid fixture metadata"))?;
    let json::MultipartMetadata {
        name,
        update: mut metadata,
        expected,
    } = json::multipart_metadata(&bytes, None, ExpectedHashes::default())?;
    metadata.acl = acl;
    let content_type = match (metadata.content_type.value(), &provenance) {
        (Some(content_type), _) => content_type.clone(),
        (None, Provenance::Seed) => config.seed_content_type().for_name(&name).into(),
        (None, Provenance::Fixture(_)) => content_type::DEFAULT.to_owned(),
    };
    let name = ObjectName::new(name).map_err(failure)?;
    metadata.validate()?;
    Ok(Fixture {
        bucket,
        name,
        provenance,
        notification_mode,
        publication: Publication {
            content_type,
            metadata,
            expected,
            condition: None,
            max_bytes: config.max_object_bytes(),
            mode: PublishMode::Replace,
        },
    })
}

/// Fields a direct fixture may set because a startup seed would otherwise choose them.
const IDENTITY_FIELDS: [&str; 4] = ["generation", "created", "timeCreated", "updated"];

fn provenance(fields: &mut Map<String, Value>, now: SystemTime) -> Result<Provenance, ApiError> {
    match fields.remove("defaults").as_ref().map(Value::as_str) {
        None | Some(Some("fixture")) => identity(fields, now).map(Provenance::Fixture),
        Some(Some("seed")) => match IDENTITY_FIELDS
            .into_iter()
            .find(|field| fields.contains_key(*field))
        {
            // A seed publication allocates its generation and stamps its own times, so an
            // explicit identity would be silently replaced.
            Some(field) => Err(ApiError::invalid_argument(format!(
                "Fixture defaults seed assigns generation and times at publication; omit {field}"
            ))),
            None => Ok(Provenance::Seed),
        },
        Some(_) => Err(ApiError::invalid_argument(
            "Fixture defaults must be fixture or seed",
        )),
    }
}

fn identity(
    fields: &mut Map<String, Value>,
    now: SystemTime,
) -> Result<ImportedIdentity, ApiError> {
    let generation = fields
        .remove("generation")
        .map(|value| {
            let generation = value
                .as_str()
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| (1..i64::MAX).contains(value))
                .ok_or_else(|| {
                    ApiError::invalid_argument(
                        "Fixture generation must be a positive i64 string below i64::MAX",
                    )
                })?;
            Generation::new(generation).map_err(failure)
        })
        .transpose()?;
    if fields.contains_key("created") && fields.contains_key("timeCreated") {
        return Err(ApiError::invalid_argument(
            "Use either created or timeCreated, not both",
        ));
    }
    let created = fields
        .remove("created")
        .or_else(|| fields.remove("timeCreated"));
    let created = timestamp(created.as_ref(), now)?;
    let updated = timestamp(fields.remove("updated").as_ref(), now)?;
    ImportedIdentity::fixture(generation, created, updated).map_err(failure)
}

/// The JSON API ignores members it does not know and non-boolean holds; this emulator-only
/// route stays strict so a mistyped fixture field is reported instead of dropped.
fn check_fixture_members(fields: &serde_json::Map<String, Value>) -> Result<(), ApiError> {
    const KNOWN: &[&str] = &[
        "kind",
        "id",
        "selfLink",
        "mediaLink",
        "name",
        "bucket",
        "generation",
        "metageneration",
        "md5Hash",
        "crc32c",
        "storageClass",
        "timeStorageClassUpdated",
        "componentCount",
        "owner",
        "softDeleteTime",
        "hardDeleteTime",
        "retentionExpirationTime",
        "restoreToken",
        "customerEncryption",
        "contentType",
        "acl",
        "contentEncoding",
        "contentDisposition",
        "contentLanguage",
        "cacheControl",
        "metadata",
        "eventBasedHold",
        "temporaryHold",
        "customTime",
        "retention",
        "notificationMode",
        "defaults",
        "timeFinalized",
    ];
    if let Some(unknown) = fields.keys().find(|key| !KNOWN.contains(&key.as_str())) {
        return Err(ApiError::unsupported_object(unknown));
    }
    for hold in ["temporaryHold", "eventBasedHold"] {
        if fields.get(hold).is_some_and(|value| !value.is_boolean()) {
            return Err(ApiError::invalid_argument(format!(
                "{hold} must be a boolean"
            )));
        }
    }
    Ok(())
}

fn fixture_acl(acl: &Value) -> Result<crate::acl::AclRequest, ApiError> {
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
                return Err(ApiError::unsupported_object(&format!("acl.{field}")));
            }
        }
    }
    let bytes = serde_json::to_vec(&serde_json::json!({"acl": acl}))
        .map_err(|_| ApiError::invalid_argument("Invalid fixture ACL"))?;
    Ok(
        json::parse_object_metadata_mutation(&bytes, json::WriteMode::Patch)?
            .update
            .acl,
    )
}

fn timestamp(value: Option<&Value>, default: SystemTime) -> Result<SystemTime, ApiError> {
    let micros = if let Some(value) = value {
        crate::import::codec::parse_timestamp(value.as_str().ok_or_else(|| {
            ApiError::invalid_argument("Fixture timestamps must be RFC3339 strings")
        })?)
        .map_err(ApiError::invalid_argument)?
    } else {
        let elapsed = default
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ApiError::invalid_argument("Fixture clock precedes Unix epoch"))?;
        i64::try_from(elapsed.as_micros())
            .map_err(|_| ApiError::invalid_argument("Fixture clock is out of range"))?
    };
    let duration = Duration::from_micros(micros.unsigned_abs());
    if micros < 0 {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    }
    .ok_or_else(|| ApiError::invalid_argument("Fixture timestamp is out of range"))
}

pub(super) fn failure(error: AdminError) -> ApiError {
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
        AdminError::Invalid(message) => return ApiError::invalid_argument(message),
        AdminError::Hold => (
            StatusCode::FORBIDDEN,
            "forbidden",
            "Existing object is protected by a hold or retention",
        ),
        AdminError::Forbidden => return AuthFailure::Forbidden.into(),
        AdminError::Busy { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "backendError",
            "Fixture publication waited longer than catalog_wait_timeout_ms for other catalog work and made no changes; retry after that work completes, or raise catalog_wait_timeout_ms (0 disables it)",
        ),
        // Seed-only refusals cannot reach a fixture, and a changed source, a closing catalog or a
        // storage failure is the server's state, not the request's.
        AdminError::DiskFull(_)
        | AdminError::Closed
        | AdminError::SeedProjectMismatch
        | AdminError::SeedMetadataBudget { .. }
        | AdminError::SourceChanged { .. }
        | AdminError::Corrupt(_)
        | AdminError::Io(_)
        | AdminError::BodyAborted(_)
        | AdminError::Backend(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "backendError",
            "Fixture publication failed; inspect local storage and retry",
        ),
    };
    ApiError::new(status, reason, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_past_the_catalog_wait_names_the_setting_instead_of_blaming_storage() {
        let error = failure(AdminError::Busy {
            retry_after: Duration::from_secs(1),
        });
        assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message().contains("catalog_wait_timeout_ms"));
    }
}
