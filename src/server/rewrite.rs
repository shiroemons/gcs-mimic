use super::*;
use crate::{
    json::rewrite::{RewriteRequest, RewriteSide},
    storage::{
        DerivedDestination,
        rewrite::{RewriteBinding, RewriteError, RewriteStart},
    },
};
use serde_json::Value;

fn error(error: RewriteError) -> JsonError {
    match error {
        RewriteError::Invalid => {
            JsonError::invalid_parameter("Invalid rewrite token or mismatched rewrite request")
        }
        RewriteError::Gone => JsonError::new(
            StatusCode::GONE,
            "gone",
            "Rewrite token is unavailable or expired",
        ),
        RewriteError::Object(error) => error.into(),
    }
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: RewriteRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, JsonError> {
    let ObjectCtx {
        state,
        config,
        activity,
        ..
    } = ctx;
    let source_access = ctx.authorize(auth::OBJECTS_GET, &operation.path[0]).await?;
    let destination_access = ctx
        .authorize(auth::OBJECTS_CREATE, &operation.path[2])
        .await?;
    if operation
        .parameters
        .contains_key("destinationPredefinedAcl")
    {
        let record = state
            .catalog
            .get_bucket(&operation.path[2])
            .await?
            .ok_or(CatalogError::NotFound)?;
        json::reject_predefined_acl(
            request.uri().query(),
            record.uniform_bucket_level_access_locked_time.is_some(),
            true,
        )?;
    }
    json::validate_read_condition_headers(request.headers(), false)?;
    json::validate_keyed_object_headers(
        request.headers(),
        json::ObjectRead::Metadata,
        &[crate::csek::DESTINATION_PREFIX, crate::csek::SOURCE_PREFIX],
    )?;
    let keys = Keys {
        destination: json::request_key(request.headers())?,
        source: json::source_key(request.headers())?,
    };
    for name in [
        "x-goog-hash",
        "content-md5",
        "x-goog-upload-command",
        "x-upload-content-type",
        "x-upload-content-length",
    ] {
        if request.headers().contains_key(name) {
            return Err(JsonError::unsupported_object(name));
        }
    }
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let body = if bytes.is_empty() {
        None
    } else {
        Some(serde_json::from_slice::<Value>(&bytes).map_err(|_| {
            JsonError::new(
                StatusCode::BAD_REQUEST,
                "parseError",
                "Invalid rewrite metadata JSON",
            )
        })?)
    };
    let body = body.filter(|value| !value.is_null());
    activity.send_modify(|progress| progress.awaiting_catalog = true);
    // Once mutation starts, await its durable result instead of timing out a detached commit.
    let result = tokio::spawn(execute(
        operation,
        body,
        keys,
        source_access,
        destination_access,
        state.catalog.clone(),
        config.max_object_bytes,
        state.api_base_url.clone(),
    ))
    .await;
    activity.send_modify(|progress| progress.awaiting_catalog = false);
    let (value, mut owner) = result
        .map_err(|failure| JsonError::from(ObjectError::Unavailable(failure.to_string())))??;
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    let response = boxed(response);
    if let Some(owner) = &mut owner {
        owner.disarm();
    }
    Ok(response)
}

/// Customer-supplied keys: `source` decrypts the source, `destination` encrypts the result.
/// A rewrite to a new destination key on the same object rotates its key.
#[derive(Clone, Copy)]
struct Keys {
    destination: Option<crate::csek::CustomerKey>,
    source: Option<crate::csek::CustomerKey>,
}

#[allow(
    clippy::too_many_arguments,
    reason = "The detached rewrite owns its request, keys and access context"
)]
async fn execute(
    operation: RewriteRequest,
    body: Option<Value>,
    keys: Keys,
    source_access: BucketAccess,
    destination_access: BucketAccess,
    catalog: Catalog,
    max_bytes: u64,
    base: String,
) -> Result<(Value, Option<crate::storage::DerivedRequestGuard>), JsonError> {
    let mut owner = None;
    let token = if let Some(token) = &operation.token {
        let session = catalog.rewrite_session(token).await.map_err(error)?;
        if !session.binding.accepts(
            [
                &operation.path[0],
                &operation.path[1],
                &operation.path[2],
                &operation.path[3],
            ],
            &operation.parameters,
            body.as_ref(),
        ) {
            return Err(error(RewriteError::Invalid));
        }
        // Each call must carry the keys the session started with.
        crate::csek::check_data(session.source_key_sha256, keys.source)?;
        crate::csek::check_data(
            session.destination.metadata.customer_key_sha256,
            keys.destination,
        )?;
        token.clone()
    } else {
        let source = catalog
            .resolve_derived_source_keyed(
                &operation.path[0],
                &operation.path[1],
                operation.number("sourceGeneration"),
                operation.conditions(RewriteSide::Source),
                &source_access,
                keys.source,
            )
            .await?;
        let mut destination = destination(&source.object, &operation, &body)?;
        destination.metadata.customer_key_sha256 =
            keys.destination.map(crate::csek::CustomerKey::sha256);
        catalog
            .check_upload(
                &operation.path[2],
                &operation.path[3],
                operation.conditions(RewriteSide::Destination),
                &destination_access,
            )
            .await?;
        let source_bucket = catalog
            .get_bucket(&operation.path[0])
            .await?
            .ok_or(ObjectError::BucketNotFound)?;
        let destination_bucket = catalog
            .get_bucket(&operation.path[2])
            .await?
            .ok_or(ObjectError::BucketNotFound)?;
        let multiple = source_bucket.location != destination_bucket.location
            || destination
                .storage_class
                .as_deref()
                .unwrap_or(&destination_bucket.storage_class)
                != source.object.storage_class;
        let budget = if multiple {
            operation
                .number("maxBytesRewrittenPerCall")
                .map(|number| number as u64)
                .unwrap_or(8 * 1024 * 1024)
        } else {
            u64::MAX
        };
        let mut parameters = operation.parameters.clone();
        for (key, value) in [
            ("projection", "noAcl"),
            ("alt", "json"),
            ("prettyPrint", "false"),
        ] {
            parameters
                .entry(key)
                .or_insert_with(|| Value::String(value.into()));
        }
        let binding = RewriteBinding {
            source_bucket: operation.path[0].clone(),
            source_object: operation.path[1].clone(),
            source_generation: source.object.generation,
            destination_bucket: operation.path[2].clone(),
            destination_object: operation.path[3].clone(),
            parameters,
            metadata: body.clone(),
        };
        let start = RewriteStart {
            binding,
            destination,
            source_conditions: operation.conditions(RewriteSide::Source),
            source_metageneration: source.object.metageneration,
            source_access: source_access.clone(),
            source_key_sha256: keys.source.map(crate::csek::CustomerKey::sha256),
            budget,
        };
        let (token, guard) = catalog
            .begin_rewrite_session(vec![source.whole()], max_bytes, start)
            .await
            .map_err(error)?;
        owner = Some(guard);
        token
    };
    let progress = catalog
        .continue_rewrite(&token, source_access, destination_access, move |record| {
            json::object_resource_keyed(record, &base, json::Projection::NoAcl, keys.destination)
                .map_err(|e| ObjectError::Unavailable(e.to_string()))
        })
        .await
        .map_err(error)?;
    let value = json::rewrite::response(progress)?;
    Ok((value, owner))
}

fn destination(
    source: &crate::object::ObjectRecord,
    request: &RewriteRequest,
    body: &Option<Value>,
) -> Result<DerivedDestination, JsonError> {
    let bytes = body.as_ref().map(Value::to_string).unwrap_or_default();
    let (metadata, class, supplied) = json::derived_metadata(bytes.as_bytes(), source)?;
    if supplied.md5.is_some_and(|hash| hash != source.md5)
        || supplied.crc32c.is_some_and(|hash| hash != source.crc32c)
    {
        return Err(ObjectError::ChecksumMismatch.into());
    }
    let content_type = metadata
        .content_type
        .as_ref()
        .and_then(|value| value.as_ref())
        .cloned()
        .unwrap_or_else(|| "application/octet-stream".into());
    Ok(DerivedDestination {
        component_count: source.component_count,
        bucket: request.path[2].clone(),
        name: request.path[3].clone(),
        content_type,
        metadata,
        storage_class: class,
        expected: crate::object::ExpectedHashes {
            md5: Some(source.md5),
            crc32c: Some(source.crc32c),
        },
        conditions: request.conditions(RewriteSide::Destination),
        access: BucketAccess::Permissive,
    })
}
