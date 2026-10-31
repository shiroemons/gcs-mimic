use super::*;
use crate::bucket::BucketAccess;

pub(super) fn start(root: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><{root} xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">"
    )
}

pub(super) fn target(body: &mut String, target: &storage::Target) {
    element(body, "Bucket", &target.bucket);
    element(body, "Key", &target.name);
    element(body, "UploadId", &target.id);
}

fn class(upload: &storage::Upload) -> &str {
    upload
        .options
        .metadata
        .storage_class
        .as_deref()
        .unwrap_or("STANDARD")
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
            let marker = numeric(&mut query, "part-number-marker", 0, 10000)? as u16;
            let limit = numeric(&mut query, "max-parts", 1000, u32::MAX)?.min(1000) as usize;
            if limit == 0 {
                return Err(invalid("max-parts must be positive"));
            }
            remaining(&query)?;
            let target = storage::Target {
                bucket: path.bucket,
                name: path.name,
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
            let mut body = start("ListPartsResult");
            target_body(&mut body, &upload, &parts, marker, limit, more)?;
            body.push_str("</ListPartsResult>");
            Ok(response(body))
        }
        Operation::Uploads => {
            let limit = numeric(&mut query, "max-uploads", 1000, u32::MAX)?.min(1000) as usize;
            if limit == 0 {
                return Err(invalid("max-uploads must be positive"));
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
                return Err(invalid("Invalid multipart listing marker or filter"));
            }
            remaining(&query)?;
            let page = ctx
                .await_catalog(ctx.state.catalog.list_xml_uploads(
                    storage::UploadQuery {
                        bucket: path.bucket.clone(),
                        prefix: prefix.clone(),
                        delimiter: delimiter.clone(),
                        key_marker: marker.clone(),
                        upload_marker: upload_marker.clone(),
                        limit,
                    },
                    access,
                ))
                .await
                .map_err(|error| storage_error(error, ctx))?;
            let mut body = start("ListMultipartUploadsResult");
            for (name, value) in [
                ("Bucket", path.bucket),
                ("KeyMarker", marker),
                ("UploadIdMarker", upload_marker.unwrap_or_default()),
                ("Prefix", prefix),
                ("Delimiter", delimiter),
                ("MaxUploads", limit.to_string()),
                ("IsTruncated", page.more.to_string()),
            ] {
                element(&mut body, name, &value);
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
            Ok(response(body))
        }
        _ => Err(invalid("Invalid multipart listing operation")),
    }
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "forwards the catalog page's IsTruncated wire value without selecting a mode"
)]
fn target_body(
    body: &mut String,
    upload: &storage::Upload,
    parts: &[storage::Part],
    marker: u16,
    limit: usize,
    more: bool,
) -> Result<(), Error> {
    target(body, &upload.target);
    element(body, "StorageClass", class(upload));
    element(body, "PartNumberMarker", &marker.to_string());
    element(body, "MaxParts", &limit.to_string());
    element(body, "IsTruncated", &more.to_string());
    if more && let Some(part) = parts.last() {
        element(body, "NextPartNumberMarker", &part.number.to_string());
    }
    for part in parts {
        body.push_str("<Part>");
        element(body, "PartNumber", &part.number.to_string());
        element(body, "LastModified", &json::timestamp(part.updated)?);
        element(body, "ETag", &part.etag);
        element(body, "Size", &part.size.to_string());
        body.push_str("</Part>");
    }
    Ok(())
}
