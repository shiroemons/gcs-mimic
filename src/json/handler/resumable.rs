//! The JSON API's resumable uploads: the initiation, the requests on its session URI, and the
//! answers a completed session makes from the resource it stores. The engine behind every
//! session URI is `service::resumable`.

use super::json_response;
use crate::json::JsonErrorResponse;
use crate::object::ObjectKey;
use crate::service::body::{RequestBody as Incoming, ResponseBody, boxed, empty_response};
use crate::service::context::ObjectCtx;
use crate::service::object_headers;
use crate::service::resumable::{
    self, AnswerView, CompletionAnswer, Links, SessionTarget, SessionUri, Transfer,
};
use crate::{
    bucket::{BucketAccess, name::BucketKey},
    json::ApiError,
    json::{self, ObjectInsertParams, resumable_cancel::Cancellation},
    service::ServiceError,
    storage::{ResumableError, ResumableOptions},
};
use hyper::{Request, Response, StatusCode, header::HeaderValue};

pub(super) async fn initiate(
    request: Request<Incoming>,
    bucket: &BucketKey,
    name: Option<String>,
    params: ObjectInsertParams,
    encoding: Option<String>,
    access: BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, ApiError> {
    let ObjectCtx { config, .. } = ctx;
    let response_fields = ctx.uri.parameters()?.get("fields").cloned();
    let json::resumable::UploadHeaders { mime, total } =
        json::resumable::headers(request.headers(), config.max_object_bytes())?;
    let key = crate::csek::request_key(request.headers())?;
    let origin = object_headers::single(request.headers(), "origin")?.map(str::to_owned);
    let metadata_type =
        object_headers::single(request.headers(), "content-type")?.map(str::to_owned);
    let json_type = metadata_type.as_ref().is_some_and(|v| {
        v.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case("application/json")
    });
    if super::has_body(request.headers()) && !json_type {
        return Err(ApiError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let base = ctx.endpoints.api_base_url();
    let prefix = json::resumable::session_path_prefix(&request);
    let location_base = format!("{base}{prefix}{bucket}/o");
    let query = request.uri().query().unwrap_or_default().to_owned();
    location_base
        .parse::<hyper::header::HeaderValue>()
        .map_err(|_| ApiError::invalid_parameter("Invalid session endpoint"))?;
    let bytes = crate::bounded_body::read_body(
        request.into_body(),
        crate::bounded_body::RESOURCE_BODY_LIMIT,
    )
    .await?;
    if !bytes.is_empty() && !json_type {
        return Err(ApiError::invalid_parameter(
            "Metadata requires application/json",
        ));
    }
    let initiation = json::resumable::Initiation {
        name,
        mime,
        params,
        access,
        origin,
        encoding,
    };
    let (name, mut options) = json::resumable::options(&bytes, initiation)?;
    options.response_fields = response_fields;
    // The session binds the key fingerprint; later chunks cannot change it.
    options.metadata.customer_key_sha256 = key.map(crate::csek::CustomerKey::sha256);
    let key = ObjectKey::from_bucket(bucket.as_key(), &name);
    let id = resumable::create_session(ctx, key, options, total).await?;
    let location = session_location(&location_base, &query, id);
    let mut response = empty_response(StatusCode::OK);
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static(crate::mime::TEXT_PLAIN_UTF8),
    );
    response.headers_mut().insert(
        "location",
        location
            .parse()
            .map_err(|_| ApiError::invalid_parameter("Invalid session endpoint"))?,
    );
    // Cloud Storage forbids every cache for the initiation reply, as for other JSON API calls.
    json::forbid_caching(response.headers_mut());
    Ok(boxed(response))
}

/// The session URI Cloud Storage returned: the initiation's own query parameters plus the
/// `upload_id`, sorted by name (recorded for `name`, `ifGenerationMatch` and `uploadType`).
fn session_location(base: &str, query: &str, id: crate::storage::SessionId) -> String {
    let mut pairs: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("upload_id="))
        .collect();
    let upload_id = format!("upload_id={id}");
    pairs.push(&upload_id);
    pairs.sort_by(|a, b| a.split('=').next().cmp(&b.split('=').next()));
    format!("{base}?{}", pairs.join("&"))
}

pub(super) async fn session_response(
    request: Request<Incoming>,
    bucket: &str,
    id: &str,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, ApiError> {
    if request.method() == hyper::Method::DELETE {
        let cancellation = ctx
            .await_session_writer(json::resumable_cancel::cancel_response(
                request,
                bucket,
                id,
                &ctx.state.catalog,
            ))
            .await?;
        return Ok(match cancellation {
            Cancellation::Cancelled => {
                boxed(ApiError::from(ResumableError::Cancelled).into_response())
            }
            Cancellation::Completed { resource, options } => {
                let resource = options
                    .view()?
                    .respond(ctx, resource, Links::Stored)
                    .await?;
                boxed(json_response(&resource))
            }
        });
    }
    let target = SessionTarget {
        bucket,
        id,
        uri: SessionUri::Json,
    };
    Ok(match resumable::transfer(request, target, ctx).await? {
        // The shared 308 carries `no-store` for the XML adapter; the JSON front end sends the
        // status reply with no cache field (recorded for an empty status request).
        Transfer::Incomplete(mut response) => {
            response.headers_mut().remove("cache-control");
            response
        }
        // The cache fields of a stored-object response come from the JSON API envelope.
        Transfer::Complete(value) => boxed(json_response(&value)),
    })
}

/// How each response of a completed session is made from the resource it stores. Cloud Storage
/// answered the completing request and a later status request with the full projection's `acl`
/// and `owner` when the initiation asked for it (recorded 2026-11-17, case
/// obj-full-projection-writes), so those fields are added to each response as the object has
/// them then, before the initiation's field selection. A refused cancellation answers the same
/// way by inference.
pub(crate) struct CompletionView {
    /// With `projection=full`, the initiator's stored access, which decides whether an answer
    /// may carry the ACL fields: requests on the session URI have no caller of their own.
    acl_reader: Option<BucketAccess>,
    fields: Option<json::Fields>,
}

impl CompletionAnswer for ResumableOptions {
    type View = CompletionView;

    /// Parses the initiation's field selection, so a bad one fails before any byte is stored.
    fn view(&self) -> Result<CompletionView, ServiceError> {
        Ok(CompletionView {
            acl_reader: (self.projection == crate::acl::Projection::Full)
                .then(|| self.access.clone()),
            fields: self
                .response_fields
                .as_deref()
                .map(json::Fields::parse_syntax)
                .transpose()?,
        })
    }
}

impl AnswerView for CompletionView {
    async fn respond(
        &self,
        ctx: ObjectCtx<'_>,
        mut resource: serde_json::Value,
        links: Links,
    ) -> Result<serde_json::Value, ServiceError> {
        match links {
            Links::ThisRequest => {}
            Links::Stored => super::rebase_stored_links(ctx.endpoints, &mut resource),
        }
        if let Some(initiator) = &self.acl_reader {
            super::acl::add_full_projection_to_stored(
                ctx.state,
                crate::service::access::acl::AclReader::Initiator(initiator),
                &mut resource,
                ctx.endpoints.api_base_url(),
            )
            .await?;
        }
        if let Some(fields) = &self.fields {
            fields.apply(&mut resource);
        }
        Ok(resource)
    }
}
