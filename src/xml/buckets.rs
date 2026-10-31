//! XML bucket operations reuse the shared project and bucket authorization decisions.

use super::{Error, ErrorBody, ObjectPath, ReadRequest, element, signing_parameter};
use crate::{
    auth, json,
    object::ObjectConditions,
    server::{RequestBody, RequestCtx, ResponseBody, anonymous_error, authorize_bucket, boxed},
};
use http_body_util::Full;
use hyper::{HeaderMap, Method, Request, Response, StatusCode, Uri, body::Bytes};

mod create;

pub(crate) fn matches(method: &Method, uri: &Uri, selected: Option<&ObjectPath>) -> bool {
    (selected.is_none() && *method == Method::GET && uri.path() == "/")
        || (selected.is_none()
            && *method == Method::PUT
            && !super::reserved_path(uri.path())
            && uri.path().strip_prefix('/').is_some_and(|path| {
                let path = path.strip_suffix('/').unwrap_or(path);
                !path.is_empty() && !path.contains('/')
            }))
        || selected.is_some_and(|path| {
            path.name().is_empty() && matches!(*method, Method::HEAD | Method::PUT | Method::DELETE)
        })
}

pub(crate) async fn respond(
    mut request: Request<RequestBody>,
    selected: Option<&ObjectPath>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    let body_mode = ErrorBody::for_method(request.method());
    let resource = selected.map_or(
        auth::signature::Resource::PathStyle,
        ObjectPath::signature_resource,
    );
    let result = match super::authenticate_target(
        &ctx.state.auth,
        &ctx.state.catalog,
        request.method(),
        request.uri(),
        request.headers(),
        ctx.state.clock.as_ref(),
        &resource,
    )
    .await
    {
        Ok(caller) => match super::normalize_legacy_headers(request.headers_mut()) {
            Ok(()) => execute(request, selected, ctx, caller).await,
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    };
    result.unwrap_or_else(|error| boxed(error.response(body_mode)))
}

async fn execute(
    request: Request<RequestBody>,
    selected: Option<&ObjectPath>,
    ctx: RequestCtx<'_>,
    caller: Option<auth::Caller<'_>>,
) -> Result<Response<ResponseBody>, Error> {
    if request.method() == Method::PUT {
        let bucket = match selected {
            Some(path) => path.bucket.clone(),
            None => json::decode(request.uri().path().trim_matches('/'), json::Decoding::Path)?,
        };
        json::validate_bucket_name_for_profile(&bucket, ctx.config.compat_profile)?;
        return create::respond(request, &bucket, ctx, caller).await;
    }
    if let Some(path) = selected {
        json::validate_bucket_name_for_profile(&path.bucket, ctx.config.compat_profile)?;
        let permission = if request.method() == Method::DELETE {
            auth::BUCKETS_DELETE
        } else {
            auth::BUCKETS_GET
        };
        let access = authorize_bucket(ctx.state, caller, permission, &path.bucket)
            .await
            .map_err(|error| anonymous_error(error, caller))?;
        validate(&request, &path.bucket)?;
        let delete = request.method() == Method::DELETE;
        super::empty_body(request.into_body()).await?;
        if delete {
            ctx.state
                .catalog
                .delete_bucket_authorized(&path.bucket, None, None, &access)
                .await
                .map_err(|error| catalog_error(error, caller))?;
            let mut response = Response::new(Full::new(Bytes::new()));
            *response.status_mut() = StatusCode::NO_CONTENT;
            return Ok(boxed(response));
        }
        ctx.state
            .catalog
            .get_bucket_authorized(&path.bucket, &access)
            .await
            .map_err(json::JsonError::from)
            .map_err(|error| anonymous_error(error, caller))?
            .ok_or_else(|| Error::new(StatusCode::NOT_FOUND, "NoSuchBucket", "Bucket not found"))?;
        return Ok(boxed(Response::new(Full::new(Bytes::new()))));
    }
    let project = project(request.headers())?;
    authorize_project(caller, auth::BUCKETS_LIST, project)?;
    validate(&request, "")?;
    let project = project.to_owned();
    super::empty_body(request.into_body()).await?;
    list(ctx, &project).await
}

fn project(headers: &HeaderMap) -> Result<&str, Error> {
    let project = super::put::single(headers, "x-goog-project-id")?.ok_or_else(|| {
        Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            "Specify x-goog-project-id; a default interoperable project is not configured.",
        )
    })?;
    json::validate_project(project)?;
    Ok(project)
}

fn authorize_project(
    caller: Option<auth::Caller<'_>>,
    permission: &str,
    project: &str,
) -> Result<(), Error> {
    if let Some(caller) = caller {
        caller
            .authorize(permission, Some(project))
            .map_err(json::JsonError::from)
            .map_err(|error| anonymous_error(error, Some(caller)))?;
    }
    Ok(())
}

fn catalog_error(error: crate::bucket::CatalogError, caller: Option<auth::Caller<'_>>) -> Error {
    match error {
        crate::bucket::CatalogError::NotEmpty => Error::new(
            StatusCode::CONFLICT,
            "BucketNotEmpty",
            "Bucket is not empty",
        ),
        crate::bucket::CatalogError::AlreadyExists => Error::new(
            StatusCode::CONFLICT,
            "BucketNameUnavailable",
            "Bucket already exists",
        ),
        error => anonymous_error(json::JsonError::from(error), caller).into(),
    }
}

fn validate(request: &Request<RequestBody>, bucket: &str) -> Result<(), Error> {
    validate_query(request.uri(), bucket)?;
    let mut headers = request.headers().clone();
    if bucket.is_empty() {
        headers.remove("x-goog-project-id");
    }
    validate_headers(&headers)
}

fn validate_query(uri: &Uri, bucket: &str) -> Result<(), Error> {
    let query = json::parse_query(uri.query().unwrap_or_default())?;
    if !bucket.is_empty()
        && uri.path() == "/"
        && query
            .keys()
            .any(|key| auth::signature::V2_PARAMETERS.contains(&key.as_str()))
    {
        return Err(Error::unsupported(
            "V2 virtual-host bucket requests; use V4 signing",
        ));
    }
    if let Some(key) = query.keys().find(|key| !signing_parameter(key)) {
        return Err(Error::unsupported(key));
    }
    Ok(())
}

fn validate_headers(headers: &HeaderMap) -> Result<(), Error> {
    if let Some(key) = headers.keys().find(|key| {
        key.as_str().starts_with("x-amz-") && !auth::signature::authentication_header(key.as_str())
    }) {
        return Err(Error::unsupported(key.as_str()));
    }
    let mut operation = ReadRequest {
        bucket: String::new(),
        name: String::new(),
        generation: None,
        conditions: ObjectConditions::default(),
        response_overrides: HeaderMap::new(),
    };
    super::headers(headers, &mut operation)?;
    if operation.conditions != ObjectConditions::default()
        || headers.contains_key("range")
        || crate::csek::has_headers(headers, crate::csek::DESTINATION_PREFIX).is_some()
    {
        return Err(Error::unsupported(
            "object conditions, ranges or encryption on a bucket",
        ));
    }
    Ok(())
}

async fn list(ctx: RequestCtx<'_>, project: &str) -> Result<Response<ResponseBody>, Error> {
    let mut body = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListAllMyBucketsResult xmlns=\"http://doc.s3.amazonaws.com/2006-03-01\"><Buckets>",
    );
    let mut count = 0;
    let mut after = None;
    while count < 1000 {
        // Metadata expansion has its own page budget; never load the whole project catalog.
        let page = ctx
            .state
            .catalog
            .list_buckets_page(project, None, after.as_deref(), 128.min(1000 - count))
            .await
            .map_err(json::JsonError::from)?;
        for bucket in &page.items {
            body.push_str("<Bucket>");
            element(&mut body, "Name", &bucket.name);
            element(
                &mut body,
                "CreationDate",
                &json::timestamp(bucket.time_created)?,
            );
            body.push_str("</Bucket>");
        }
        count += page.items.len();
        after = page.items.last().map(|bucket| bucket.name.clone());
        if !page.has_more || after.is_none() {
            break;
        }
    }
    body.push_str("</Buckets></ListAllMyBucketsResult>");
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/xml; charset=UTF-8"),
    );
    Ok(boxed(response))
}
