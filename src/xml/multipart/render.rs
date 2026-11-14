use super::*;
use crate::bucket::BucketAccess;

#[cfg(test)]
mod tests;

pub(super) fn start(root: &str) -> String {
    format!(
        "<?xml version='1.0' encoding='UTF-8'?><{root} xmlns='http://s3.amazonaws.com/doc/2006-03-01/'>"
    )
}

pub(super) fn target(body: &mut String, target: &storage::Target) {
    element(body, "Bucket", &target.bucket);
    element(body, "Key", &target.name);
    element(body, "UploadId", &target.id);
}

pub(super) fn initiate_body(target: &storage::Target) -> String {
    let mut body = start("InitiateMultipartUploadResult");
    self::target(&mut body, target);
    body.push_str("</InitiateMultipartUploadResult>");
    body
}

pub(super) fn complete_body(api_base_url: &str, record: &crate::object::ObjectRecord) -> String {
    let mut body = start("CompleteMultipartUploadResult");
    element(
        &mut body,
        "Location",
        &format!(
            "{}/{}/{}",
            api_base_url.trim_end_matches('/'),
            record.bucket,
            crate::url::encode_object_name(&record.name)
        ),
    );
    element(&mut body, "Bucket", &record.bucket);
    element(&mut body, "Key", &record.name);
    element(&mut body, "ETag", &super::super::object_etag(record));
    body.push_str("</CompleteMultipartUploadResult>");
    body
}

fn class(upload: &storage::Upload) -> &str {
    upload
        .options
        .metadata
        .storage_class
        .unwrap_or(crate::bucket::StorageClass::Standard)
        .as_str()
}

pub(super) async fn list(
    request: Request<RequestBody>,
    path: ReadRequest,
    operation: Operation,
    id: String,
    mut query: BTreeMap<String, String>,
    access: BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let headers = request_headers(&request, 0, operation)?;
    if headers.expected != Default::default() {
        return Err(Error::unsupported(
            "checksums on a multipart listing request",
        ));
    }
    if crate::csek::has_headers(request.headers(), crate::csek::DESTINATION_PREFIX).is_some() {
        return Err(Error::unsupported(
            "encryption headers on multipart listing",
        ));
    }
    empty_body(request, headers.sha256).await?;
    match operation {
        Operation::Parts => {
            let marker = u16::try_from(numeric(&mut query, "part-number-marker", 0, 10000)?)
                .map_err(|_| Error::invalid("Multipart number is out of range"))?;
            let limit = numeric(&mut query, "max-parts", 1000, u32::MAX)?.min(1000) as usize;
            if limit == 0 {
                return Err(Error::invalid("max-parts must be positive"));
            }
            remaining(&query)?;
            let target = storage::Target {
                bucket: path.target.bucket,
                name: path.target.name,
                id,
            };
            let (upload, parts, more) = ctx
                .await_catalog(
                    ctx.state
                        .catalog
                        .list_xml_parts(target, access, marker, limit),
                )
                .await
                .map_err(|error| storage_error(error, ctx))?;
            Ok(response(parts_body(&upload, &parts, marker, limit, more)?))
        }
        Operation::Uploads => {
            let limit = numeric(&mut query, "max-uploads", 1000, u32::MAX)?.min(1000) as usize;
            if limit == 0 {
                return Err(Error::invalid("max-uploads must be positive"));
            }
            let prefix = query.remove("prefix").unwrap_or_default();
            let delimiter = query.remove("delimiter").unwrap_or_default();
            let marker = query.remove("key-marker").unwrap_or_default();
            let upload_marker = query.remove("upload-id-marker");
            for value in [
                &prefix,
                &delimiter,
                &marker,
                upload_marker.as_deref().unwrap_or_default(),
            ] {
                xml_text(value)?;
            }
            if prefix.len() > 1024
                || delimiter.len() > 1024
                || marker.len() > 1024
                || upload_marker
                    .as_ref()
                    .is_some_and(|value| value.len() > 1024 || marker.is_empty())
            {
                return Err(Error::invalid("Invalid multipart listing marker or filter"));
            }
            remaining(&query)?;
            let query = storage::UploadQuery {
                bucket: path.target.bucket,
                prefix,
                delimiter,
                key_marker: marker,
                upload_marker,
                limit,
            };
            let page = ctx
                .await_catalog(ctx.state.catalog.list_xml_uploads(query.clone(), access))
                .await
                .map_err(|error| storage_error(error, ctx))?;
            Ok(response(uploads_body(&query, page)?))
        }
        _ => Err(Error::invalid("Invalid multipart listing operation")),
    }
}

pub(super) fn uploads_body(
    query: &storage::UploadQuery,
    page: storage::UploadPage,
) -> Result<String, Error> {
    let mut body = start("ListMultipartUploadsResult");
    let limit = query.limit.to_string();
    let more = page.more.to_string();
    for (name, value) in [
        ("Bucket", query.bucket.as_str()),
        ("KeyMarker", query.key_marker.as_str()),
        (
            "UploadIdMarker",
            query.upload_marker.as_deref().unwrap_or(""),
        ),
        ("Prefix", query.prefix.as_str()),
        ("Delimiter", query.delimiter.as_str()),
        ("MaxUploads", limit.as_str()),
        ("IsTruncated", more.as_str()),
    ] {
        element(&mut body, name, value);
    }
    if page.more {
        element(&mut body, "NextKeyMarker", &page.next_key);
        element(&mut body, "NextUploadIdMarker", &page.next_upload);
    }
    for upload in page.uploads {
        xml_text(&upload.target.name)?;
        body.push_str("<Upload>");
        element(&mut body, "Key", &upload.target.name);
        element(&mut body, "UploadId", &upload.target.id);
        element(&mut body, "StorageClass", class(&upload));
        element(&mut body, "Initiated", &json::timestamp(upload.created)?);
        body.push_str("</Upload>");
    }
    for prefix in page.prefixes {
        body.push_str("<CommonPrefixes>");
        element(&mut body, "Prefix", &prefix);
        body.push_str("</CommonPrefixes>");
    }
    body.push_str("</ListMultipartUploadsResult>");
    Ok(body)
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "forwards the catalog page's IsTruncated wire value without selecting a mode"
)]
pub(super) fn parts_body(
    upload: &storage::Upload,
    parts: &[storage::Part],
    marker: u16,
    limit: usize,
    more: bool,
) -> Result<String, Error> {
    let mut body = start("ListPartsResult");
    target(&mut body, &upload.target);
    element(&mut body, "StorageClass", class(upload));
    element(&mut body, "PartNumberMarker", &marker.to_string());
    element(&mut body, "MaxParts", &limit.to_string());
    element(&mut body, "IsTruncated", &more.to_string());
    if more && let Some(part) = parts.last() {
        element(&mut body, "NextPartNumberMarker", &part.number.to_string());
    }
    for part in parts {
        body.push_str("<Part>");
        element(&mut body, "PartNumber", &part.number.to_string());
        element(&mut body, "LastModified", &json::timestamp(part.updated)?);
        element(&mut body, "ETag", &part.etag);
        element(&mut body, "Size", &part.size.to_string());
        body.push_str("</Part>");
    }
    body.push_str("</ListPartsResult>");
    Ok(body)
}
