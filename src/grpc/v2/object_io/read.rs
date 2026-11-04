use super::resource::{OwnerFields, mask_object};
use super::*;

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

pub(super) fn read_range(size: u64, offset: i64, limit: i64) -> Result<(u64, u64), Status> {
    if limit < 0 {
        return Err(Status::invalid_argument(
            "read_limit must be nonnegative; use 0 to read to the end",
        ));
    }
    validate::resolve_range(size, offset, limit).ok_or_else(|| {
        Status::out_of_range("read_offset exceeds object size; use 0 or a valid byte offset")
    })
}

impl Storage {
    pub(in crate::grpc::v2) async fn get_object(
        &self,
        request: Request<pb::GetObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        if r.soft_deleted == Some(true) || !r.restore_token.is_empty() {
            return Err(unsupported_for(
                "GetObject soft_deleted/restore_token",
                caller,
            ));
        }
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let access = self.access(caller, bucket, OBJECTS_GET).await?;
        let target = ObjectTarget::new(bucket, name, generation(r.generation)?);
        let (record, finality) = self
            .get_readable(target, request_conditions!(conditions, r)?, &access)
            .await
            .map_err(named_object_status(bucket, name))?;
        let mut object = super::super::append::object(&record, finality)?;
        if finality == Finality::Finalized {
            self.add_owner_fields(
                caller,
                bucket,
                std::slice::from_mut(&mut object),
                OwnerFields::selected(r.read_mask.as_ref(), None),
            )
            .await?;
        }
        Ok(Response::new(mask_object(object, r.read_mask.as_ref())?))
    }
    pub(in crate::grpc::v2) async fn read_object(
        &self,
        request: Request<pb::ReadObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::ReadObjectResponse>>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let access = self.access(caller, bucket, OBJECTS_GET).await?;
        let target = ObjectTarget::new(bucket, name, generation(r.generation)?);
        let (record, mut reader, finality) = self
            .open_readable(target, request_conditions!(conditions, r)?, &access)
            .await
            .map_err(named_object_status(bucket, name))?;
        check_read_offset(&record, r.read_offset)?;
        let (start, length) = read_range(record.size, r.read_offset, r.read_limit)?;
        reader
            .set_range(start, length)
            .await
            .map_err(read_failure)?;
        let mut object = super::super::append::object(&record, finality)?;
        if finality == Finality::Finalized {
            self.add_owner_fields(
                caller,
                bucket,
                std::slice::from_mut(&mut object),
                OwnerFields::selected(r.read_mask.as_ref(), Some("metadata")),
            )
            .await?;
        }
        let mut first = pb::ReadObjectResponse {
            metadata: Some(object.clone()),
            object_checksums: object.checksums,
            content_range: (r.read_offset != 0 || r.read_limit != 0).then_some(pb::ContentRange {
                start: crate::grpc::convert::signed(start, "read range start")?,
                end: crate::grpc::convert::signed(
                    start.checked_add(length).ok_or_else(|| {
                        ObjectError::Corrupt("Stored read range end overflows".into())
                    })?,
                    "read range end",
                )?,
                complete_length: crate::grpc::convert::signed(record.size, "size")?,
            }),
            ..Default::default()
        };
        if let Some(mask) = r.read_mask
            && !mask.paths.is_empty()
            && !mask.paths.iter().any(|p| p == "*")
        {
            let mut paths = Vec::new();
            let mut metadata = false;
            for path in &mask.paths {
                match path.as_str() {
                    "checksummed_data"
                    | "checksummed_data.content"
                    | "checksummed_data.crc32c"
                    | "object_checksums"
                    | "content_range" => {}
                    "metadata" => metadata = true,
                    p if let Some(field) = p.strip_prefix("metadata.") => {
                        paths.push(field.to_owned())
                    }
                    _ => {
                        return Err(unsupported_for(
                            &format!("ReadObject read_mask {path}"),
                            caller,
                        ));
                    }
                }
            }
            if !metadata {
                first.metadata = if paths.is_empty() {
                    None
                } else {
                    first
                        .metadata
                        .take()
                        .map(|metadata| {
                            mask_object(metadata, Some(&prost_types::FieldMask { paths }))
                        })
                        .transpose()?
                };
            }
            if !mask.paths.iter().any(|p| p == "object_checksums") {
                first.object_checksums = None;
            }
            if !mask.paths.iter().any(|p| p == "content_range") {
                first.content_range = None;
            }
        }
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
        let bucket = self.bucket_id(&r.parent)?;
        if r.page_size < 0 {
            return Err(Status::invalid_argument(
                "page_size must be nonnegative; use 1000",
            ));
        }
        if !r.filter.is_empty() {
            return Err(unsupported_for("ListObjects filter", caller));
        }
        let limit = if r.page_size == 0 {
            1000
        } else {
            usize::try_from(r.page_size).unwrap_or(1000).min(1000)
        };
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
        };
        query
            .validate(if r.include_folders_as_prefixes {
                crate::storage::FoldersAsPrefixes::Requested
            } else {
                crate::storage::FoldersAsPrefixes::Omitted
            })
            .map_err(super::super::status::list_query_status)?;
        if !r.page_token.is_empty() {
            query.cursor = Some(json::decode_object_page_token(&r.page_token, &query, json::Projection::NoAcl).map_err(|_| Status::invalid_argument("Invalid page_token; reuse the token with the original prefix, delimiter and other listing fields"))?);
        }
        let access = self.access(caller, bucket, OBJECTS_LIST).await?;
        let page = self
            .state
            .catalog
            .list_objects(query.clone(), limit, &access)
            .await
            .map_err(object_status)?;
        let next_page_token = page
            .next
            .as_ref()
            .map(|cursor| json::encode_object_page_token(&query, json::Projection::NoAcl, cursor))
            .unwrap_or_default();
        let mut objects = page
            .items
            .iter()
            .map(convert::object)
            .collect::<Result<Vec<_>, _>>()?;
        self.add_owner_fields(
            caller,
            bucket,
            &mut objects,
            OwnerFields::selected(r.read_mask.as_ref(), None),
        )
        .await?;
        let objects = objects
            .into_iter()
            .map(|object| mask_object(object, r.read_mask.as_ref()))
            .collect::<Result<Vec<_>, Status>>()?;
        Ok(Response::new(pb::ListObjectsResponse {
            objects,
            prefixes: page.prefixes,
            next_page_token,
        }))
    }
}
