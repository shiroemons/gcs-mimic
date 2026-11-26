//! XML bucket operations reuse the shared project and bucket authorization decisions.

use super::{
    Error, ErrorBody, HeaderKind, Namespace, ObjectPath, XmlCode, XmlWriter, accepted_parameter,
};
use crate::{
    auth,
    object::ObjectConditions,
    server::{RequestBody, RequestCtx, ResponseBody, boxed, bucket_access, empty_response},
};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, Uri};

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
    request: Request<RequestBody>,
    selected: Option<&ObjectPath>,
    ctx: RequestCtx<'_>,
) -> Response<ResponseBody> {
    let body_mode = ErrorBody::for_method(request.method());
    authenticate_and_execute(request, selected, ctx)
        .await
        .unwrap_or_else(|error| boxed(error.response(body_mode)))
}

/// Authenticates against the request as sent, then normalizes its AWS extension headers.
async fn authenticate_and_execute(
    mut request: Request<RequestBody>,
    selected: Option<&ObjectPath>,
    ctx: RequestCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let resource = selected.map_or(
        auth::signature::Resource::PathStyle,
        ObjectPath::signature_resource,
    );
    let caller = super::authenticate_target(ctx.xml_authentication(&request, &resource)).await?;
    super::extension_headers::normalize(&mut request, super::extension_headers::Route::Bucket)?;
    execute(request, selected, ctx, caller).await
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
            None => crate::percent::decode(
                request.uri().path().trim_matches('/'),
                crate::percent::Decoding::Path,
            )?,
        };
        crate::bucket::name::validate_for_profile(&bucket, ctx.config.compat_profile())?;
        return create::respond(request, &bucket, ctx, caller).await;
    }
    if let Some(path) = selected {
        crate::bucket::name::validate_for_profile(&path.bucket, ctx.config.compat_profile())?;
        let permission = if request.method() == Method::DELETE {
            auth::BUCKETS_DELETE
        } else {
            auth::BUCKETS_GET
        };
        let access = bucket_access(ctx.state, caller, permission, &path.bucket)
            .await
            .map_err(|error| Error::from(error).for_caller(caller))?;
        validate(&request, &path.bucket)?;
        let delete = request.method() == Method::DELETE;
        super::empty_body(request.into_body()).await?;
        if delete {
            ctx.state
                .catalog
                .delete_bucket(&path.bucket, Default::default(), &access)
                .await
                .map_err(|error| catalog_error(error, caller))?;
            return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
        }
        ctx.state
            .catalog
            .get_bucket(&path.bucket, &access)
            .await
            .map_err(|error| Error::from(error).for_caller(caller))?
            .ok_or_else(|| Error::new(XmlCode::NoSuchBucket, "Bucket not found"))?;
        return Ok(boxed(empty_response(StatusCode::OK)));
    }
    let project = project_id(ctx, request.headers(), caller).await?;
    authorize_project(caller, auth::BUCKETS_LIST, &project)?;
    validate(&request, "")?;
    super::empty_body(request.into_body()).await?;
    list(ctx, &project).await
}

/// The ID of the project `x-goog-project-id` names by ID or by number.
async fn project_id(
    ctx: RequestCtx<'_>,
    headers: &HeaderMap,
    caller: Option<auth::Caller<'_>>,
) -> Result<String, Error> {
    ctx.state
        .project_id(project(headers)?)
        .await
        .map_err(|error| catalog_error(error, caller))
}

fn project(headers: &HeaderMap) -> Result<&str, Error> {
    let project = super::put::single(headers, "x-goog-project-id")?.ok_or_else(|| {
        Error::invalid(
            "Specify x-goog-project-id; a default interoperable project is not configured.",
        )
    })?;
    crate::auth::validate_project_id(project)?;
    Ok(project)
}

fn authorize_project(
    caller: Option<auth::Caller<'_>>,
    permission: auth::Permission,
    project: &str,
) -> Result<(), Error> {
    if let Some(caller) = caller {
        caller
            .authorize(permission, Some(project))
            .map_err(|error| Error::from(error).for_caller(Some(caller)))?;
    }
    Ok(())
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other catalog error keeps the XML adapter's catalog mapping"
)]
fn catalog_error(error: crate::bucket::CatalogError, caller: Option<auth::Caller<'_>>) -> Error {
    match error {
        crate::bucket::CatalogError::NotEmpty => {
            Error::new(XmlCode::BucketNotEmpty, "Bucket is not empty")
        }
        crate::bucket::CatalogError::AlreadyExists => {
            Error::new(XmlCode::BucketNameUnavailable, "Bucket already exists")
        }
        error => Error::from(error).for_caller(caller),
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
    let query = crate::percent::parse_query(uri.query().unwrap_or_default())?;
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
    if let Some(key) = query.keys().find(|key| !accepted_parameter(key)) {
        return Err(Error::unsupported(key));
    }
    Ok(())
}

fn validate_headers(headers: &HeaderMap) -> Result<(), Error> {
    if let Some(key) = headers.keys().find(|key| {
        matches!(
            HeaderKind::of(key.as_str()),
            HeaderKind::Amz | HeaderKind::DecodedLength
        )
    }) {
        return Err(Error::unsupported(key.as_str()));
    }
    let conditions = super::headers(headers, super::HeaderPolicy::Strict)?;
    if conditions != ObjectConditions::default()
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
    let mut document = XmlWriter::new("ListAllMyBucketsResult", Namespace::Listing);
    // Each page is read across an `.await`, so the list's tags are written apart.
    document.open("Buckets");
    let mut count = 0;
    let mut after = None;
    while count < 1000 {
        // Metadata expansion has its own page budget; never load the whole project catalog.
        let page = ctx
            .state
            .catalog
            .list_buckets_page(project, None, after.as_deref(), 128.min(1000 - count))
            .await?;
        for bucket in &page.items {
            document.try_group("Bucket", |w| {
                w.element("Name", &bucket.name);
                w.element(
                    "CreationDate",
                    &crate::time_format::timestamp(bucket.time_created)?,
                );
                Ok::<_, Error>(())
            })?;
        }
        count += page.items.len();
        after = page.items.last().map(|bucket| bucket.name.clone());
        if !page.has_more || after.is_none() {
            break;
        }
    }
    document.close("Buckets");
    Ok(boxed(super::document_response(document.finish())))
}
