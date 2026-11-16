use super::super::{Namespace, XmlWriter, name_encoding::NameEncoding};
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

/// Cloud Storage answered an `https` completion with an `http` `Location` (recorded
/// 2026-11-16), so the advertised base keeps its host and path but not its scheme.
pub(super) fn complete_body(api_base_url: &str, record: &crate::object::ObjectRecord) -> String {
    let base = api_base_url
        .split_once("://")
        .map_or(api_base_url, |(_, rest)| rest)
        .trim_end_matches('/');
    let mut w = XmlWriter::new("CompleteMultipartUploadResult", Namespace::Multipart);
    w.element(
        "Location",
        &format!(
            "http://{base}/{}/{}",
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
    listing: Listing,
    mut query: BTreeMap<String, String>,
    access: BucketAccess,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let headers = request_headers(
        &request,
        0,
        put::Framing::OptionalEmpty,
        InitiationMetadata::Refused,
    )?;
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
    match listing {
        Listing::Parts { upload_id } => {
            let marker = part_marker(&mut query)?;
            let limit = page_limit(&mut query, "max-parts")?;
            remaining(&query)?;
            let target = upload_target(path, upload_id);
            let page = ctx
                .await_catalog(
                    ctx.state
                        .catalog
                        .list_xml_parts(target, access, marker, limit),
                )
                .await
                .map_err(|error| storage_error(error, ctx))?;
            Ok(listing_document(parts_body(
                &page.upload,
                &page.parts,
                marker,
                limit,
                page.more,
            )?))
        }
        Listing::Uploads => {
            let request = UploadsRequest::parse(path.target.bucket, &mut query)?;
            let page = ctx
                .await_catalog(ctx.state.catalog.list_xml_uploads(request.query(), access))
                .await
                .map_err(|error| storage_error(error, ctx))?;
            Ok(listing_document(uploads_body(&request, page)?))
        }
    }
}

/// A multipart upload listing request, read once from its query.
pub(super) struct UploadsRequest {
    pub(super) bucket: String,
    /// As sent: the answer echoes `Prefix` and `Delimiter` only when the request had them.
    pub(super) prefix: Option<String>,
    pub(super) delimiter: Option<String>,
    pub(super) key_marker: String,
    pub(super) upload_marker: Option<String>,
    pub(super) limit: usize,
    pub(super) encoding: NameEncoding,
}

impl UploadsRequest {
    fn parse(bucket: String, query: &mut BTreeMap<String, String>) -> Result<Self, Error> {
        let limit = page_limit(query, "max-uploads")?;
        let encoding = match query.remove("encoding-type").as_deref() {
            None => NameEncoding::Plain,
            Some("url") => NameEncoding::Form,
            Some(other) => {
                return Err(Error::invalid_argument(format!(
                    "Invalid encoding type: {other}"
                )));
            }
        };
        let prefix = query.remove("prefix");
        let delimiter = query.remove("delimiter");
        let key_marker = query.remove("key-marker").unwrap_or_default();
        // Cloud Storage ignores an upload ID marker without a key marker: it listed from the
        // start and echoed an empty UploadIdMarker (recorded 2026-11-16).
        let upload_marker = query
            .remove("upload-id-marker")
            .filter(|_| !key_marker.is_empty());
        let values = [
            prefix.as_deref(),
            delimiter.as_deref(),
            upload_marker.as_deref(),
        ];
        for value in values.into_iter().flatten().chain([key_marker.as_str()]) {
            xml_text(value)?;
            if value.len() > 1024 {
                return Err(Error::invalid("Invalid multipart listing marker or filter"));
            }
        }
        remaining(query)?;
        Ok(Self {
            bucket,
            prefix,
            delimiter,
            key_marker,
            upload_marker,
            limit,
            encoding,
        })
    }

    /// What the catalog searches: an absent filter matches like an empty one.
    fn query(&self) -> storage::UploadQuery {
        storage::UploadQuery {
            bucket: self.bucket.clone(),
            prefix: self.prefix.clone().unwrap_or_default(),
            delimiter: self.delimiter.clone().unwrap_or_default(),
            key_marker: self.key_marker.clone(),
            upload_marker: self.upload_marker.clone(),
            limit: self.limit,
        }
    }
}

/// The recorded order: the next markers are always present (empty on the last page) and
/// enclose the `Prefix` and `Delimiter` echoes, and `EncodingType` comes last. With
/// `encoding-type=url`, every name and name echo is encoded; only `Key` was recorded with a
/// character that changes (a space as `+`).
pub(super) fn uploads_body(
    request: &UploadsRequest,
    page: storage::UploadPage,
) -> Result<String, Error> {
    let encoding = request.encoding;
    let (next_key, next_upload) = if page.more {
        (page.next_key.as_str(), page.next_upload.as_str())
    } else {
        ("", "")
    };
    let mut w = XmlWriter::new("ListMultipartUploadsResult", Namespace::Multipart);
    w.element("Bucket", &request.bucket);
    encoding.element(&mut w, "KeyMarker", &request.key_marker);
    w.element(
        "UploadIdMarker",
        request.upload_marker.as_deref().unwrap_or_default(),
    );
    encoding.element(&mut w, "NextKeyMarker", next_key);
    if let Some(prefix) = &request.prefix {
        encoding.element(&mut w, "Prefix", prefix);
    }
    if let Some(delimiter) = &request.delimiter {
        encoding.element(&mut w, "Delimiter", delimiter);
    }
    w.element("NextUploadIdMarker", next_upload);
    w.value("MaxUploads", request.limit);
    w.value("IsTruncated", page.more);
    for upload in page.uploads {
        xml_text(&upload.target.name)?;
        w.try_group("Upload", |w| {
            encoding.element(w, "Key", &upload.target.name);
            w.element("UploadId", &upload.target.id);
            w.element("StorageClass", class(&upload));
            w.element("Initiated", &json::timestamp_micros(upload.created)?);
            Ok::<_, Error>(())
        })?;
    }
    for prefix in page.prefixes {
        w.group("CommonPrefixes", |w| encoding.element(w, "Prefix", &prefix));
    }
    encoding.write_encoding_type(&mut w);
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
    // NextPartNumberMarker is always present and was 0 on an untruncated page (recorded
    // 2026-11-16); a truncated page names its last part, which was not recorded.
    let next = parts.last().filter(|_| more).map_or(0, |part| part.number);
    let mut w = XmlWriter::new("ListPartsResult", Namespace::Multipart);
    target(&mut w, &upload.target);
    w.element("StorageClass", class(upload));
    w.value("PartNumberMarker", marker);
    w.value("NextPartNumberMarker", next);
    w.value("MaxParts", limit);
    w.value("IsTruncated", more);
    for part in parts {
        w.try_group("Part", |w| {
            w.value("PartNumber", part.number);
            w.element("LastModified", &json::timestamp_micros(part.updated)?);
            w.element("ETag", &part.etag);
            w.value("Size", part.size);
            Ok::<_, Error>(())
        })?;
    }
    Ok(w.finish())
}
