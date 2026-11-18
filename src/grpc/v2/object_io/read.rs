use super::resource::{OwnerFields, mask_object};
use super::*;
use crate::grpc::{
    v2::{customer_key, mask::ReadMask},
    validate::{ByteRange, RangeError},
};
use crate::storage::ReadableMetadata;

/// Cloud Storage names the object and its size when a read starts past the end;
/// [`read_range`] keeps the generic bound check.
fn check_read_offset(record: &crate::object::ObjectRecord, offset: i64) -> Result<(), Status> {
    if u64::try_from(offset).is_ok_and(|offset| offset > record.size) {
        return Err(Status::out_of_range(format!(
            "Invalid read offset '{offset}' for object 'projects/_/buckets/{}/{}#{}'. The read offset must not be greater than the object's size ({} bytes).",
            record.bucket, record.name, record.generation, record.size
        )));
    }
    Ok(())
}

/// Cloud Storage rejected a ReadObject whose read mask named only `metadata.checksums.md5_hash`
/// (recorded): a mask that selects fields must select the object bytes too. An absent or empty
/// mask and `*` select every field. Selecting the whole `checksummed_data` message is assumed to
/// count, since it contains `content`; only the rejection itself is recorded.
fn require_content_in_read_mask(mask: ReadMask<'_>) -> Result<(), Status> {
    let Some(paths) = mask.selective_paths() else {
        return Ok(());
    };
    if paths.iter().any(|path| {
        matches!(
            path.as_str(),
            "checksummed_data" | "checksummed_data.content"
        )
    }) {
        return Ok(());
    }
    Err(crate::grpc::v2::status::invalid_with_reason(
        format!(
            "The 'read_mask' field must include checksummed_data.content, but was '{}'.",
            paths.join(",")
        ),
        "GRPC_READ_MASK_MISSING_CHECKSUMMED_DATA",
    ))
}

/// Checks the object fields a read mask names before the RPC authorizes the caller or looks
/// anything up, as GetBucket checks its mask. Which fields a mask may name does not depend on the
/// object, so an invalid mask is reported for a missing object or an empty listing too.
/// [`mask_object`] applies these same rules, so this runs it on an empty object. ListBuckets
/// checks its mask later, after it authorizes the project.
fn check_object_mask(mask: ReadMask<'_>) -> Result<(), Status> {
    mask_object(pb::Object::default(), mask).map(drop)
}

/// A ReadObject read mask, checked with the request before the caller is authorized or the
/// object opened. Its `metadata.` paths select fields of the object (`metadata` alone selects
/// all of it); the response's own fields are kept only when named.
struct FirstResponseMask<'a> {
    /// The mask's paths, or `None` when it keeps every field.
    paths: Option<&'a [String]>,
    /// The object fields the `metadata.` paths name, or `None` when `metadata` alone keeps all
    /// of them.
    object: Option<Vec<String>>,
}

impl<'a> FirstResponseMask<'a> {
    fn parse(mask: ReadMask<'a>, caller: Option<Caller<'_>>) -> Result<Self, Status> {
        require_content_in_read_mask(mask)?;
        let Some(paths) = mask.selective_paths() else {
            return Ok(Self {
                paths: None,
                object: None,
            });
        };
        let mut object = Vec::new();
        let mut metadata = false;
        for path in paths {
            match path.as_str() {
                "checksummed_data"
                | "checksummed_data.content"
                | "checksummed_data.crc32c"
                | "object_checksums"
                | "content_range" => {}
                "metadata" => metadata = true,
                p if let Some(field) = p.strip_prefix("metadata.") => object.push(field.to_owned()),
                _ => {
                    return Err(unsupported_for(
                        &format!("ReadObject read_mask {path}"),
                        caller,
                    ));
                }
            }
        }
        let object = (!metadata).then_some(object);
        if let Some(object) = &object {
            check_object_mask(ReadMask::from(object.as_slice()))?;
        }
        Ok(Self {
            paths: Some(paths),
            object,
        })
    }

    fn apply(&self, first: &mut pb::ReadObjectResponse) -> Result<(), Status> {
        let Some(paths) = self.paths else {
            return Ok(());
        };
        if let Some(object) = &self.object {
            let object_mask = ReadMask::from(object.as_slice());
            first.metadata = match object_mask {
                ReadMask::Default => None,
                ReadMask::Wildcard | ReadMask::MixedWildcard(_) | ReadMask::Paths(_) => first
                    .metadata
                    .take()
                    .map(|metadata| mask_object(metadata, object_mask))
                    .transpose()?,
            };
        }
        if !paths.iter().any(|p| p == "object_checksums") {
            first.object_checksums = None;
        }
        if !paths.iter().any(|p| p == "content_range") {
            first.content_range = None;
        }
        Ok(())
    }
}

pub(super) fn read_range(size: u64, offset: i64, limit: i64) -> Result<ByteRange, Status> {
    validate::resolve_range(size, offset, limit).map_err(|error| match error {
        RangeError::NegativeLength => {
            Status::invalid_argument("read_limit must be nonnegative; use 0 to read to the end")
        }
        RangeError::StartsPastEnd => {
            Status::out_of_range("read_offset exceeds object size; use 0 or a valid byte offset")
        }
    })
}

impl Storage {
    pub(in crate::grpc::v2) async fn get_object(
        &self,
        request: Request<pb::GetObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        let mask = ReadMask::from(r.read_mask.as_ref());
        // A malformed key fails before the object is looked up (recorded 2026-11-15).
        let key = customer_key::key(r.common_object_request_params.as_ref())?;
        if r.soft_deleted == Some(true) || !r.restore_token.is_empty() {
            return Err(unsupported_for(
                "GetObject soft_deleted/restore_token",
                caller,
            ));
        }
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        check_object_mask(mask)?;
        let access = self.access(caller, bucket, OBJECTS_GET).await?;
        let target =
            ObjectTarget::new(bucket, name, generation(r.generation)?.map(Generation::get));
        let ReadableMetadata {
            record,
            finality,
            hashes,
        } = self
            .get_readable(
                target,
                conditions(ObjectConditions::from(&r))?,
                &access,
                key,
            )
            .await
            .map_err(named_object_status(bucket, name))?;
        let mut object = super::super::append::object(&record, finality, hashes)?;
        if finality == Finality::Finalized {
            self.add_owner_fields(
                caller,
                bucket,
                std::slice::from_mut(&mut object),
                OwnerFields::selected(mask, None),
            )
            .await?;
        }
        Ok(Response::new(mask_object(object, mask)?))
    }
    pub(in crate::grpc::v2) async fn read_object(
        &self,
        request: Request<pb::ReadObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::ReadObjectResponse>>, Status> {
        let r = request.into_inner();
        let key = customer_key::key(r.common_object_request_params.as_ref())?;
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let mask = ReadMask::from(r.read_mask.as_ref());
        let first_mask = FirstResponseMask::parse(mask, caller)?;
        let access = self.access(caller, bucket, OBJECTS_GET).await?;
        let target =
            ObjectTarget::new(bucket, name, generation(r.generation)?.map(Generation::get));
        let (record, mut reader, finality) = self
            .open_readable(
                target,
                conditions(ObjectConditions::from(&r))?,
                &access,
                key,
            )
            .await
            .map_err(named_object_status(bucket, name))?;
        check_read_offset(&record, r.read_offset)?;
        let range = read_range(record.size, r.read_offset, r.read_limit)?;
        reader
            .set_range(range.start, range.len)
            .await
            .map_err(read_failure)?;
        // Opening the data verified the key (an encrypted object without one fails), so the
        // stored bytes' hashes are always the caller's to see.
        let mut object =
            super::super::append::object(&record, finality, crate::csek::StoredHashes::Revealed)?;
        if finality == Finality::Finalized {
            self.add_owner_fields(
                caller,
                bucket,
                std::slice::from_mut(&mut object),
                OwnerFields::selected(mask, Some("metadata")),
            )
            .await?;
        }
        let mut first = pb::ReadObjectResponse {
            metadata: Some(object.clone()),
            object_checksums: object.checksums,
            content_range: (r.read_offset != 0 || r.read_limit != 0).then_some(pb::ContentRange {
                start: crate::grpc::convert::signed(range.start, "read range start")?,
                end: crate::grpc::convert::signed(
                    range.start.checked_add(range.len).ok_or_else(|| {
                        ObjectError::Corrupt("Stored read range end overflows".into())
                    })?,
                    "read range end",
                )?,
                complete_length: crate::grpc::convert::signed(record.size, "size")?,
            }),
            ..Default::default()
        };
        first_mask.apply(&mut first)?;
        let (tx, rx) = mpsc::channel(1);
        crate::task::spawn_supervised("grpc-read-object", async move {
            // Cloud Storage ends the stream after the last data message instead of appending an
            // empty one (observed 2026-11-01); an empty object still gets its metadata message.
            let mut pending = Some(first);
            loop {
                let (bytes, done) = match reader.next_chunk().await {
                    Ok(Some(bytes)) => (bytes, false),
                    Ok(None) if pending.is_some() => (Default::default(), true),
                    Ok(None) => break,
                    Err(error) => {
                        let _ = tx.send(Err(read_failure(error))).await;
                        break;
                    }
                };
                let mut response = pending.take().unwrap_or_default();
                response.checksummed_data = Some(pb::ChecksummedData {
                    crc32c: Some(crc32c::crc32c(&bytes)),
                    content: bytes,
                });
                if tx.send(Ok(response)).await.is_err() || done {
                    break;
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
    pub(in crate::grpc::v2) async fn list_objects(
        &self,
        request: Request<pb::ListObjectsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListObjectsResponse>, Status> {
        let r = request.into_inner();
        let mask = ReadMask::from(r.read_mask.as_ref());
        let bucket = self.bucket_id(&r.parent)?;
        let limit = validate::page_size(r.page_size)
            .ok_or_else(|| Status::invalid_argument("page_size must be nonnegative; use 1000"))?;
        if !r.filter.is_empty() {
            return Err(unsupported_for("ListObjects filter", caller));
        }
        let mut query = crate::storage::ObjectListQuery {
            bucket: bucket.into(),
            prefix: r.prefix,
            delimiter: r.delimiter,
            match_glob: (!r.match_glob.is_empty())
                .then(|| crate::match_glob::Glob::parse(&r.match_glob))
                .transpose()
                .map_err(|error| super::super::status::glob_status(&error))?,
            start_offset: r.lexicographic_start,
            end_offset: r.lexicographic_end,
            include_trailing_delimiter: r.include_trailing_delimiter,
            versions: r.versions,
            soft_deleted: r.soft_deleted,
            cursor: None,
            page_bytes: Some(crate::grpc::LIST_PAGE_BYTES),
        };
        query
            .validate(if r.include_folders_as_prefixes {
                crate::storage::FoldersAsPrefixes::Requested
            } else {
                crate::storage::FoldersAsPrefixes::Omitted
            })
            .map_err(super::super::status::list_query_status)?;
        if !r.page_token.is_empty() {
            query.cursor = Some(json::decode_object_page_token(&r.page_token, &query).map_err(|_| Status::invalid_argument("Invalid page_token; use a next_page_token from an earlier listing response"))?);
        }
        check_object_mask(mask)?;
        let access = self.access(caller, bucket, OBJECTS_LIST).await?;
        let page = self
            .state
            .catalog
            .list_objects(query, limit, &access)
            .await
            .map_err(object_status)?;
        let next_page_token = page
            .next
            .as_ref()
            .map(json::encode_object_page_token)
            .unwrap_or_default();
        let mut objects = page
            .items
            .iter()
            .map(|record| {
                convert::object(
                    record,
                    crate::csek::StoredHashes::keyless(record.customer_key_sha256),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.add_owner_fields(
            caller,
            bucket,
            &mut objects,
            OwnerFields::selected(mask, None),
        )
        .await?;
        let objects = objects
            .into_iter()
            .map(|object| mask_object(object, mask))
            .collect::<Result<Vec<_>, Status>>()?;
        Ok(Response::new(pb::ListObjectsResponse {
            objects,
            prefixes: page.prefixes,
            next_page_token,
        }))
    }
}

#[cfg(test)]
mod list_tests;
#[cfg(test)]
mod mask_tests;
#[cfg(test)]
mod request_refusals;
#[cfg(test)]
mod tests;
