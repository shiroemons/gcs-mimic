use super::super::{Namespace, XmlWriter};
use super::*;
use crate::bucket::BucketAccess;

#[cfg(test)]
mod tests;

fn target(w: &mut XmlWriter, target: &storage::Target) {
    w.element("Bucket", &target.bucket);
    w.element("Key", &target.name);
    w.element("UploadId", &target.id);
}

pub(super) fn initiate_body(target: &storage::Target) -> String {
    let mut w = XmlWriter::new("InitiateMultipartUploadResult", Namespace::Multipart);
    self::target(&mut w, target);
    w.finish()
}

pub(super) fn complete_body(api_base_url: &str, record: &crate::object::ObjectRecord) -> String {
    let mut w = XmlWriter::new("CompleteMultipartUploadResult", Namespace::Multipart);
    w.element(
        "Location",
        &format!(
            "{}/{}/{}",
            api_base_url.trim_end_matches('/'),
            record.bucket,
            crate::url::encode_object_name(&record.name)
        ),
    );
    w.element("Bucket", &record.bucket);
    w.element("Key", &record.name);
    w.element("ETag", &super::super::object_etag(record));
    w.finish()
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
            let page = ctx
                .await_catalog(
                    ctx.state
                        .catalog
                        .list_xml_parts(target, access, marker, limit),
                )
                .await
                .map_err(|error| storage_error(error, ctx))?;
            Ok(response(parts_body(
                &page.upload,
                &page.parts,
                marker,
                limit,
                page.more,
            )?))
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
    let mut w = XmlWriter::new("ListMultipartUploadsResult", Namespace::Multipart);
    w.element("Bucket", &query.bucket);
    w.element("KeyMarker", &query.key_marker);
    w.element(
        "UploadIdMarker",
        query.upload_marker.as_deref().unwrap_or(""),
    );
    w.element("Prefix", &query.prefix);
    w.element("Delimiter", &query.delimiter);
    w.value("MaxUploads", query.limit);
    w.value("IsTruncated", page.more);
    if page.more {
        w.element("NextKeyMarker", &page.next_key);
        w.element("NextUploadIdMarker", &page.next_upload);
    }
    for upload in page.uploads {
        xml_text(&upload.target.name)?;
        w.try_group("Upload", |w| {
            w.element("Key", &upload.target.name);
            w.element("UploadId", &upload.target.id);
            w.element("StorageClass", class(&upload));
            w.element("Initiated", &json::timestamp(upload.created)?);
            Ok::<_, Error>(())
        })?;
    }
    for prefix in page.prefixes {
        w.group("CommonPrefixes", |w| w.element("Prefix", &prefix));
    }
    Ok(w.finish())
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
    let mut w = XmlWriter::new("ListPartsResult", Namespace::Multipart);
    target(&mut w, &upload.target);
    w.element("StorageClass", class(upload));
    w.value("PartNumberMarker", marker);
    w.value("MaxParts", limit);
    w.value("IsTruncated", more);
    if more && let Some(part) = parts.last() {
        w.value("NextPartNumberMarker", part.number);
    }
    for part in parts {
        w.try_group("Part", |w| {
            w.value("PartNumber", part.number);
            w.element("LastModified", &json::timestamp(part.updated)?);
            w.element("ETag", &part.etag);
            w.value("Size", part.size);
            Ok::<_, Error>(())
        })?;
    }
    Ok(w.finish())
}
