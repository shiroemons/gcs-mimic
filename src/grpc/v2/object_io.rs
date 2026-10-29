use super::{
    ResponseStream, Storage, convert, names, pb,
    status::{from_json as json_status, unsupported_for as unsupported},
};
use crate::{
    auth::{Caller, OBJECTS_CREATE, OBJECTS_DELETE, OBJECTS_GET, OBJECTS_LIST},
    json,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, UploadChunk},
    storage::{ResumableChunk, ResumableError, ResumableOptions, SessionState},
};
use bytes::Bytes;
use prost::Message;
use tokio::sync::{mpsc, watch};
use tonic::codegen::tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};

fn object_status(error: ObjectError) -> Status {
    if matches!(error, ObjectError::UnsupportedMetadata(_)) {
        return unsupported(&error.to_string(), None);
    }
    json_status(error.into())
}

/// Anonymous denials ask for credentials, matching bucket RPCs, BidiReadObject and JSON.
fn access_status(error: json::JsonError, caller: Option<Caller<'_>>) -> Status {
    json_status(crate::server::anonymous_error(error, caller))
}

fn session_status(error: ResumableError) -> Status {
    match error {
        ResumableError::NotFound | ResumableError::Gone => {
            Status::not_found("Upload session not found; start a new resumable write")
        }
        ResumableError::Invalid(message) => Status::invalid_argument(message),
        ResumableError::Object(error) => object_status(error),
    }
}

fn common(params: Option<&pb::CommonObjectRequestParams>) -> Result<(), Status> {
    if params.is_some_and(|p| p != &pb::CommonObjectRequestParams::default()) {
        return Err(unsupported(
            "customer encryption; omit common_object_request_params",
            None,
        ));
    }
    Ok(())
}

fn conditions(
    g: Option<i64>,
    ng: Option<i64>,
    m: Option<i64>,
    nm: Option<i64>,
) -> Result<ObjectConditions, Status> {
    if [g, ng, m, nm].into_iter().flatten().any(|n| n < 0) {
        return Err(Status::invalid_argument(
            "Preconditions must be nonnegative; use if_generation_match=0 to create only",
        ));
    }
    Ok(ObjectConditions {
        if_generation_match: g,
        if_generation_not_match: ng,
        if_metageneration_match: m,
        if_metageneration_not_match: nm,
    })
}

fn generation(value: i64) -> Result<Option<i64>, Status> {
    if value < 0 {
        return Err(Status::invalid_argument(
            "generation must be positive, or 0 to select the live object",
        ));
    }
    Ok((value != 0).then_some(value))
}

fn hashes(value: Option<&pb::ObjectChecksums>) -> Result<ExpectedHashes, Status> {
    let Some(value) = value else {
        return Ok(ExpectedHashes::default());
    };
    Ok(ExpectedHashes {
        crc32c: value.crc32c,
        md5: if value.md5_hash.is_empty() {
            None
        } else {
            Some(
                value.md5_hash.as_slice().try_into().map_err(|_| {
                    Status::invalid_argument("md5_hash must contain exactly 16 bytes")
                })?,
            )
        },
    })
}

fn merge_hashes(first: ExpectedHashes, next: ExpectedHashes) -> Result<ExpectedHashes, Status> {
    if matches!((first.crc32c,next.crc32c),(Some(a),Some(b)) if a!=b)
        || matches!((first.md5,next.md5),(Some(a),Some(b)) if a!=b)
    {
        return Err(Status::invalid_argument(
            "Conflicting object_checksums; use the same digest in each message",
        ));
    }
    Ok(ExpectedHashes {
        crc32c: next.crc32c.or(first.crc32c),
        md5: next.md5.or(first.md5),
    })
}

fn mask_object(
    mut object: pb::Object,
    mask: Option<&prost_types::FieldMask>,
) -> Result<pb::Object, Status> {
    let Some(mask) = mask.filter(|m| !m.paths.is_empty() && !m.paths.iter().any(|p| p == "*"))
    else {
        return Ok(object);
    };
    if mask.paths.iter().any(|path| {
        matches!(
            path.as_str(),
            "etag"
                | "restore_token"
                | "acl"
                | "finalize_time"
                | "kms_key"
                | "update_storage_class_time"
                | "contexts"
                | "owner"
        )
    }) {
        return Err(unsupported(
            "read_mask requests an unsupported object field; omit ACL, encryption and other unimplemented fields",
            None,
        ));
    }
    macro_rules! fields { ($($field:ident),+ $(,)?) => {{
        for path in &mask.paths {
            if !matches!(path.as_str(), $(stringify!($field))|+) {
                return Err(unsupported(&format!("read_mask path {path}; request a supported top-level object field"), None));
            }
        }
        $(if !mask.paths.iter().any(|p| p == stringify!($field)) { object.$field = Default::default(); })+
    }} }
    fields!(
        name,
        bucket,
        etag,
        generation,
        restore_token,
        metageneration,
        storage_class,
        size,
        content_encoding,
        content_disposition,
        cache_control,
        acl,
        content_language,
        delete_time,
        finalize_time,
        content_type,
        create_time,
        component_count,
        checksums,
        update_time,
        kms_key,
        update_storage_class_time,
        temporary_hold,
        retention_expire_time,
        metadata,
        contexts,
        event_based_hold,
        owner,
        customer_encryption,
        custom_time,
        soft_delete_time,
        hard_delete_time,
        retention
    );
    Ok(object)
}

fn read_range(size: u64, offset: i64, limit: i64) -> Result<(u64, u64), Status> {
    if limit < 0 {
        return Err(Status::invalid_argument(
            "read_limit must be nonnegative; use 0 to read to the end",
        ));
    }
    let start = if offset < 0 {
        size.saturating_sub(offset.unsigned_abs())
    } else {
        offset as u64
    };
    if start > size {
        return Err(Status::out_of_range(
            "read_offset exceeds object size; use 0 or a valid byte offset",
        ));
    }
    Ok((
        start,
        if limit == 0 {
            size - start
        } else {
            (limit as u64).min(size - start)
        },
    ))
}

fn upload_id(bucket: &str, id: &str) -> String {
    format!("{bucket}/{id}")
}

fn parse_upload_id(id: &str) -> Result<(&str, &str), Status> {
    let (bucket, id) = id
        .split_once('/')
        .filter(|(b, i)| !b.is_empty() && i.len() == 64 && i.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| {
            Status::not_found("Unknown upload_id; use the value returned by StartResumableWrite")
        })?;
    Ok((bucket, id))
}

impl Storage {
    pub(super) async fn delete_object(
        &self,
        request: Request<pb::DeleteObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        let (bucket, name) = names::object_name(&r.bucket, &r.object)?;
        let access = crate::server::authorize_bucket(&self.state, caller, OBJECTS_DELETE, bucket)
            .await
            .map_err(|e| access_status(e, caller))?;
        self.state
            .catalog
            .delete_object_authorized(
                bucket,
                name,
                generation(r.generation)?,
                conditions(
                    r.if_generation_match,
                    r.if_generation_not_match,
                    r.if_metageneration_match,
                    r.if_metageneration_not_match,
                )?,
                &access,
            )
            .await
            .map_err(object_status)?;
        Ok(Response::new(()))
    }
    /// Shares the JSON session-URI cancellation so staged bytes follow one cleanup journal.
    pub(super) async fn cancel_resumable_write(
        &self,
        request: Request<pb::CancelResumableWriteRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::CancelResumableWriteResponse>, Status> {
        let r = request.into_inner();
        let (bucket, id) = parse_upload_id(&r.upload_id)?;
        // gRPC upload IDs are not bearer capabilities, so cancellation requires the
        // same permission as QueryWriteStatus and resumed writes.
        crate::server::authorize_bucket(&self.state, caller, OBJECTS_CREATE, bucket)
            .await
            .map_err(|e| access_status(e, caller))?;
        match self
            .state
            .catalog
            .cancel_resumable_session(id, bucket)
            .await
        {
            Ok(()) => Ok(Response::new(pb::CancelResumableWriteResponse {})),
            Err(ResumableError::Gone) => {
                // A completed object is not an in-progress upload; report that distinctly
                // from a cancelled or expired session, matching resumed WriteObject.
                let finalized = self
                    .state
                    .catalog
                    .resumable_session(id, bucket)
                    .await
                    .is_ok_and(|session| session.state == SessionState::Complete);
                Err(if finalized {
                    Status::failed_precondition(
                        "Upload already finalized; delete the object instead of cancelling",
                    )
                } else {
                    session_status(ResumableError::Gone)
                })
            }
            Err(error) => Err(session_status(error)),
        }
    }
    pub(super) async fn get_object(
        &self,
        request: Request<pb::GetObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        if r.soft_deleted == Some(true) || !r.restore_token.is_empty() {
            return Err(unsupported("GetObject soft_deleted/restore_token", caller));
        }
        let (bucket, name) = names::object_name(&r.bucket, &r.object)?;
        let access = crate::server::authorize_bucket(&self.state, caller, OBJECTS_GET, bucket)
            .await
            .map_err(|e| access_status(e, caller))?;
        let record = self
            .state
            .catalog
            .get_object_authorized(
                bucket,
                name,
                generation(r.generation)?,
                conditions(
                    r.if_generation_match,
                    r.if_generation_not_match,
                    r.if_metageneration_match,
                    r.if_metageneration_not_match,
                )?,
                &access,
            )
            .await
            .map_err(object_status)?;
        Ok(Response::new(mask_object(
            convert::object(&record)?,
            r.read_mask.as_ref(),
        )?))
    }
    pub(super) async fn read_object(
        &self,
        request: Request<pb::ReadObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::ReadObjectResponse>>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        let (bucket, name) = names::object_name(&r.bucket, &r.object)?;
        let access = crate::server::authorize_bucket(&self.state, caller, OBJECTS_GET, bucket)
            .await
            .map_err(|e| access_status(e, caller))?;
        let (record, mut reader) = self
            .state
            .catalog
            .open_object_authorized(
                bucket,
                name,
                generation(r.generation)?,
                conditions(
                    r.if_generation_match,
                    r.if_generation_not_match,
                    r.if_metageneration_match,
                    r.if_metageneration_not_match,
                )?,
                &access,
            )
            .await
            .map_err(object_status)?;
        let (start, length) = read_range(record.size, r.read_offset, r.read_limit)?;
        reader.set_range(start, length).await.map_err(|_| {
            Status::internal("Cannot seek object data; restore a valid data directory")
        })?;
        let object = convert::object(&record)?;
        let mut first = pb::ReadObjectResponse {
            metadata: Some(object.clone()),
            object_checksums: object.checksums,
            content_range: (r.read_offset != 0 || r.read_limit != 0).then_some(pb::ContentRange {
                start: start as i64,
                end: (start + length) as i64,
                complete_length: record.size as i64,
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
                    p if p.starts_with("metadata.") => paths.push(p[9..].to_owned()),
                    _ => {
                        return Err(unsupported(&format!("ReadObject read_mask {path}"), caller));
                    }
                }
            }
            if !metadata {
                first.metadata = if paths.is_empty() {
                    None
                } else {
                    Some(mask_object(
                        first.metadata.take().expect("first metadata"),
                        Some(&prost_types::FieldMask { paths }),
                    )?)
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
        tokio::spawn(async move {
            let mut response = first;
            loop {
                match reader.next_chunk().await {
                    Ok(data) => {
                        let done = data.is_none();
                        let bytes = data.unwrap_or_default();
                        response.checksummed_data = Some(pb::ChecksummedData {
                            crc32c: Some(crc32c::crc32c(&bytes)),
                            content: bytes.to_vec(),
                        });
                        if tx.send(Ok(response)).await.is_err() || done {
                            break;
                        }
                        response = pb::ReadObjectResponse::default();
                    }
                    Err(_) => {
                        let _ = tx
                            .send(Err(Status::internal(
                                "Cannot read object data; restore a valid data directory",
                            )))
                            .await;
                        break;
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
    pub(super) async fn write_object(
        &self,
        request: Request<Streaming<pb::WriteObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::WriteObjectResponse>, Status> {
        let mut stream = request.into_inner().map(|result| {
            result.map(|r| pb::BidiWriteObjectRequest {
                write_offset: r.write_offset,
                object_checksums: r.object_checksums,
                finish_write: r.finish_write,
                common_object_request_params: r.common_object_request_params,
                first_message: r.first_message.map(|f| match f {
                    pb::write_object_request::FirstMessage::UploadId(id) => {
                        pb::bidi_write_object_request::FirstMessage::UploadId(id)
                    }
                    pb::write_object_request::FirstMessage::WriteObjectSpec(spec) => {
                        pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)
                    }
                }),
                data: r
                    .data
                    .map(|pb::write_object_request::Data::ChecksummedData(data)| {
                        pb::bidi_write_object_request::Data::ChecksummedData(data)
                    }),
                ..Default::default()
            })
        });
        let result = self.receive_write(&mut stream, caller, None, true).await?;
        Ok(Response::new(pb::WriteObjectResponse {
            write_status: Some(match result {
                pb::bidi_write_object_response::WriteStatus::PersistedSize(n) => {
                    pb::write_object_response::WriteStatus::PersistedSize(n)
                }
                pb::bidi_write_object_response::WriteStatus::Resource(r) => {
                    pb::write_object_response::WriteStatus::Resource(r)
                }
            }),
            ..Default::default()
        }))
    }
    pub(super) async fn bidi_write_object(
        &self,
        request: Request<Streaming<pb::BidiWriteObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::BidiWriteObjectResponse>>, Status> {
        let mut stream = request.into_inner();
        let first = stream.message().await?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        if matches!(
            first.first_message,
            Some(pb::bidi_write_object_request::FirstMessage::AppendObjectSpec(_))
        ) || matches!(&first.first_message,Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(s)) if s.appendable==Some(true))
        {
            return Err(unsupported("Rapid append", caller));
        }
        let strict = caller.is_some();
        let identity = caller.and_then(|c| match c {
            Caller::Principal(i) => Some(i.clone()),
            Caller::Anonymous => None,
        });
        let storage = Storage {
            state: self.state.clone(),
            max_object_bytes: self.max_object_bytes,
        };
        let (tx, rx) = mpsc::channel(1);
        tokio::spawn(async move {
            let caller = strict.then(|| {
                identity
                    .as_ref()
                    .map_or(Caller::Anonymous, Caller::Principal)
            });
            let mut input = tonic::codegen::tokio_stream::once(Ok(first)).chain(stream);
            if let Err(error) = storage
                .receive_write(&mut input, caller, Some(&tx), false)
                .await
            {
                let _ = tx.send(Err(error)).await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }
    pub(super) async fn list_objects(
        &self,
        request: Request<pb::ListObjectsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::ListObjectsResponse>, Status> {
        let r = request.into_inner();
        let bucket = names::bucket_name(&r.parent)?;
        if r.page_size < 0 {
            return Err(Status::invalid_argument(
                "page_size must be nonnegative; use 1000",
            ));
        }
        if r.soft_deleted || r.include_folders_as_prefixes || !r.filter.is_empty() {
            return Err(unsupported(
                "ListObjects soft_deleted/include_folders_as_prefixes/filter",
                caller,
            ));
        }
        let encode = crate::auth::signature::encode_query_component;
        let query = [
            ("prefix", r.prefix),
            ("delimiter", r.delimiter),
            ("pageToken", r.page_token),
            ("maxResults", r.page_size.to_string()),
            ("startOffset", r.lexicographic_start),
            ("endOffset", r.lexicographic_end),
            (
                "includeTrailingDelimiter",
                r.include_trailing_delimiter.to_string(),
            ),
            ("versions", r.versions.to_string()),
            ("matchGlob", r.match_glob),
        ]
        .into_iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", encode(&v)))
        .collect::<Vec<_>>()
        .join("&");
        let uri = format!("/storage/v1/b/{}/o?{query}", encode(bucket))
            .parse()
            .map_err(|_| Status::invalid_argument("Invalid listing query"))?;
        let Some(Ok(json::ObjectRequest::List(params))) =
            json::route_object(&hyper::Method::GET, &uri)
        else {
            return Err(Status::invalid_argument(
                "Invalid listing query or page_token; reuse the token with the original prefix and delimiter",
            ));
        };
        let access = crate::server::authorize_bucket(&self.state, caller, OBJECTS_LIST, bucket)
            .await
            .map_err(|e| access_status(e, caller))?;
        let page = self
            .state
            .catalog
            .list_objects_authorized(params.query.clone(), params.max_results, &access)
            .await
            .map_err(object_status)?;
        let value =
            json::object_list(&page, &params, &self.state.api_base_url).map_err(json_status)?;
        let objects = page
            .items
            .iter()
            .map(|record| mask_object(convert::object(record)?, r.read_mask.as_ref()))
            .collect::<Result<Vec<_>, Status>>();
        Ok(Response::new(pb::ListObjectsResponse {
            objects: objects?,
            prefixes: page.prefixes,
            next_page_token: value["nextPageToken"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        }))
    }
    pub(super) async fn start_resumable_write(
        &self,
        request: Request<pb::StartResumableWriteRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::StartResumableWriteResponse>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        let spec = r
            .write_object_spec
            .ok_or_else(|| Status::invalid_argument("write_object_spec is required"))?;
        let (bucket, name, options, total) = self
            .write_spec(spec, r.object_checksums.as_ref(), caller)
            .await?;
        let session = self
            .state
            .catalog
            .create_resumable_session(&bucket, &name, options, total)
            .await
            .map_err(session_status)?;
        Ok(Response::new(pb::StartResumableWriteResponse {
            upload_id: upload_id(&bucket, &session.id),
        }))
    }
    pub(super) async fn query_write_status(
        &self,
        request: Request<pb::QueryWriteStatusRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::QueryWriteStatusResponse>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        let (bucket, id) = parse_upload_id(&r.upload_id)?;
        let access = crate::server::authorize_bucket(&self.state, caller, OBJECTS_CREATE, bucket)
            .await
            .map_err(|e| access_status(e, caller))?;
        let session = self
            .state
            .catalog
            .resumable_session(id, bucket)
            .await
            .map_err(session_status)?;
        self.state
            .catalog
            .list_objects_authorized(
                crate::storage::ObjectListQuery {
                    bucket: bucket.to_owned(),
                    prefix: session.name.clone(),
                    ..Default::default()
                },
                1,
                &access,
            )
            .await
            .map_err(object_status)?;
        let status = if session.state == SessionState::Complete {
            pb::query_write_status_response::WriteStatus::Resource(decode_completion(
                session.completion.as_ref(),
            )?)
        } else {
            pb::query_write_status_response::WriteStatus::PersistedSize(session.offset as i64)
        };
        Ok(Response::new(pb::QueryWriteStatusResponse {
            write_status: Some(status),
            ..Default::default()
        }))
    }
}

fn decode_completion(value: Option<&serde_json::Value>) -> Result<pb::Object, Status> {
    let bytes: Vec<u8> = serde_json::from_value(
        value
            .cloned()
            .ok_or_else(|| Status::internal("Missing upload completion"))?,
    )
    .map_err(|_| Status::internal("Invalid upload completion"))?;
    pb::Object::decode(bytes.as_slice()).map_err(|_| Status::internal("Invalid upload completion"))
}

impl Storage {
    async fn write_spec(
        &self,
        mut spec: pb::WriteObjectSpec,
        checksums: Option<&pb::ObjectChecksums>,
        caller: Option<Caller<'_>>,
    ) -> Result<(String, String, ResumableOptions, Option<u64>), Status> {
        if spec.appendable == Some(true) || !spec.predefined_acl.is_empty() {
            return Err(unsupported(
                "appendable/predefined_acl; omit these fields",
                caller,
            ));
        }
        let mut resource = spec
            .resource
            .take()
            .ok_or_else(|| Status::invalid_argument("write_object_spec.resource is required"))?;
        if resource.encoded_len() > 8192 {
            return Err(Status::resource_exhausted(
                "Object metadata exceeds the supported 8 KiB limit; reduce metadata",
            ));
        }
        let (bucket, name) = names::object_name(&resource.bucket, &resource.name)?;
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = crate::server::authorize_bucket(&self.state, caller, OBJECTS_CREATE, &bucket)
            .await
            .map_err(|e| access_status(e, caller))?;
        let content_type = if resource.content_type.is_empty() {
            "application/octet-stream".to_owned()
        } else {
            std::mem::take(&mut resource.content_type)
        };
        let mut metadata = ObjectMetadataUpdate {
            content_encoding: (!resource.content_encoding.is_empty())
                .then(|| Some(std::mem::take(&mut resource.content_encoding))),
            content_disposition: (!resource.content_disposition.is_empty())
                .then(|| Some(std::mem::take(&mut resource.content_disposition))),
            content_language: (!resource.content_language.is_empty())
                .then(|| Some(std::mem::take(&mut resource.content_language))),
            cache_control: (!resource.cache_control.is_empty())
                .then(|| Some(std::mem::take(&mut resource.cache_control))),
            metadata: Some(Some(
                std::mem::take(&mut resource.metadata)
                    .into_iter()
                    .map(|(k, v)| (k, Some(v)))
                    .collect(),
            )),
            temporary_hold: resource.temporary_hold.then_some(true),
            event_based_hold: resource.event_based_hold.take(),
            custom_time: resource
                .custom_time
                .take()
                .map(|time| time.to_string())
                .map(Some),
            storage_class: (!resource.storage_class.is_empty())
                .then(|| std::mem::take(&mut resource.storage_class)),
            ..Default::default()
        };
        resource.temporary_hold = false;
        resource.name.clear();
        resource.bucket.clear();
        let expected = merge_hashes(hashes(resource.checksums.as_ref())?, hashes(checksums)?)?;
        resource.checksums = None;
        if resource != pb::Object::default() {
            return Err(unsupported(
                "write_object_spec.resource fields; use content_type, fixed/custom metadata and holds",
                caller,
            ));
        }
        metadata.content_type = Some(Some(content_type.clone()));
        metadata.validate().map_err(object_status)?;
        let conditions = conditions(
            spec.if_generation_match,
            spec.if_generation_not_match,
            spec.if_metageneration_match,
            spec.if_metageneration_not_match,
        )?;
        let total = spec
            .object_size
            .map(|n| {
                u64::try_from(n)
                    .map_err(|_| Status::invalid_argument("object_size must be nonnegative"))
            })
            .transpose()?;
        if total.is_some_and(|n| n > self.max_object_bytes) {
            return Err(Status::resource_exhausted(
                "object_size exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
            ));
        }
        let mut options = ResumableOptions::new(access);
        options.content_type = content_type;
        options.metadata = metadata;
        options.expected = expected;
        options.conditions = conditions;
        Ok((bucket, name, options, total))
    }

    async fn receive_write<S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin>(
        &self,
        stream: &mut S,
        caller: Option<Caller<'_>>,
        responses: Option<&mpsc::Sender<Result<pb::BidiWriteObjectResponse, Status>>>,
        require_eof: bool,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        use pb::bidi_write_object_request::FirstMessage;
        use pb::bidi_write_object_response::WriteStatus;
        let mut message = stream.next().await.transpose()?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        common(message.common_object_request_params.as_ref())?;
        let (bucket, id) = match message.first_message.take() {
            Some(FirstMessage::WriteObjectSpec(spec)) => {
                return self
                    .receive_single_write(stream, message, spec, caller, responses, require_eof)
                    .await;
            }
            Some(FirstMessage::UploadId(upload)) => {
                let (bucket, id) = parse_upload_id(&upload)?;
                (bucket.to_owned(), id.to_owned())
            }
            Some(FirstMessage::AppendObjectSpec(_)) => {
                return Err(unsupported("Rapid append", caller));
            }
            None => {
                return Err(Status::invalid_argument(
                    "First message must contain write_object_spec or upload_id",
                ));
            }
        };
        let mut first = true;
        let mut result;
        loop {
            common(message.common_object_request_params.as_ref())?;
            if message.first_message.is_some() {
                return Err(Status::invalid_argument(
                    "Only the first message may contain write_object_spec or upload_id",
                ));
            }
            if message.object_checksums.is_some() && !message.finish_write {
                return Err(unsupported(
                    "object_checksums before resumable finish; set StartResumableWrite.object_checksums or send them with finish_write=true",
                    caller,
                ));
            }
            let access =
                crate::server::authorize_bucket(&self.state, caller, OBJECTS_CREATE, &bucket)
                    .await
                    .map_err(|e| access_status(e, caller))?;
            let lease = self
                .state
                .catalog
                .lock_resumable_session(&id, &bucket)
                .await
                .map_err(session_status)?;
            if lease.session.state == SessionState::Complete {
                return Err(Status::failed_precondition(
                    "Upload already finalized; query its status or start a new write",
                ));
            }
            let mut options = lease.session.options.clone();
            options.access = access;
            self.state
                .catalog
                .check_upload(
                    &bucket,
                    &lease.session.name,
                    options.conditions,
                    &options.access,
                )
                .await
                .map_err(object_status)?;
            options.expected =
                merge_hashes(options.expected, hashes(message.object_checksums.as_ref())?)?;
            let offset = u64::try_from(message.write_offset).map_err(|_| {
                Status::out_of_range("write_offset must be nonnegative; query the persisted offset")
            })?;
            if (first && offset != lease.session.offset) || offset > lease.session.offset {
                return Err(Status::out_of_range(
                    "write_offset does not match persisted data; call QueryWriteStatus and resume at persisted_size",
                ));
            }
            let data = match message.data.take() {
                Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
                None => pb::ChecksummedData::default(),
            };
            if data
                .crc32c
                .is_some_and(|crc| crc != crc32c::crc32c(&data.content))
            {
                return Err(Status::invalid_argument(
                    "checksummed_data.crc32c mismatch; resend the correct data and checksum",
                ));
            }
            let end = offset
                .checked_add(data.content.len() as u64)
                .ok_or_else(|| Status::out_of_range("write_offset plus content length overflows"))?
                .max(lease.session.offset);
            if end > self.max_object_bytes {
                return Err(Status::resource_exhausted(
                    "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
                ));
            }
            if lease
                .session
                .total
                .is_some_and(|total| end > total || (message.finish_write && end != total))
            {
                return Err(Status::out_of_range(
                    "Written bytes do not match object_size; start a new upload with the correct size",
                ));
            }
            if message.finish_write && require_eof && stream.next().await.transpose()?.is_some() {
                return Err(Status::invalid_argument(
                    "A message followed finish_write; finish_write must be the final message",
                ));
            }
            let (tx, rx) = mpsc::channel(2);
            let (catalog_wait, _wait) = watch::channel(false);
            let chunk = ResumableChunk {
                start: offset,
                length: data.content.len() as u64,
                total: lease.session.total.or(message.finish_write.then_some(end)),
                finalize: message.finish_write,
                options,
                catalog_wait,
            };
            let permit = self
                .state
                .catalog
                .admit_object_upload()
                .map_err(object_status)?;
            let producer = async {
                for bytes in data.content.chunks(64 * 1024) {
                    tx.send(Ok(UploadChunk::Data(Bytes::copy_from_slice(bytes))))
                        .await
                        .map_err(|_| Status::cancelled("Upload consumer stopped"))?;
                }
                tx.send(Ok(UploadChunk::End))
                    .await
                    .map_err(|_| Status::cancelled("Upload consumer stopped"))
            };
            let write =
                self.state
                    .catalog
                    .write_resumable_session(lease, chunk, rx, permit, |record| {
                        let resource =
                            convert::object(record).map_err(|_| ObjectError::InvalidMetadata)?;
                        serde_json::to_value(resource.encode_to_vec())
                            .map_err(|_| ObjectError::InvalidMetadata)
                    });
            let (sent, written) = tokio::join!(producer, write);
            let completion = written.map_err(session_status)?;
            sent?;
            result = if let Some(value) = completion.as_ref() {
                WriteStatus::Resource(decode_completion(Some(value))?)
            } else {
                WriteStatus::PersistedSize(end as i64)
            };
            if let Some(tx) = responses
                && (message.finish_write || message.flush || message.state_lookup)
            {
                tx.send(Ok(pb::BidiWriteObjectResponse {
                    write_status: Some(result.clone()),
                    ..Default::default()
                }))
                .await
                .map_err(|_| Status::cancelled("Response stream closed"))?;
            }
            if message.finish_write {
                return Ok(result);
            }
            first = false;
            let Some(next) = stream.next().await.transpose()? else {
                break;
            };
            message = next;
        }
        Ok(result)
    }

    async fn receive_single_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        mut message: pb::BidiWriteObjectRequest,
        spec: pb::WriteObjectSpec,
        caller: Option<Caller<'_>>,
        responses: Option<&mpsc::Sender<Result<pb::BidiWriteObjectResponse, Status>>>,
        require_eof: bool,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        let (bucket, name, options, total) = self
            .write_spec(spec, message.object_checksums.as_ref(), caller)
            .await?;
        let permit = self
            .state
            .catalog
            .prepare_object_upload(&bucket, &name, options.conditions, &options.access)
            .await
            .map_err(object_status)?;
        let (tx, rx) = mpsc::channel(2);
        let producer = async move {
            let mut offset = 0_u64;
            let mut first = true;
            let mut expected = options.expected;
            let mut hasher = crate::hashing::Hasher::default();
            loop {
                common(message.common_object_request_params.as_ref())?;
                if message.first_message.is_some() {
                    return Err(Status::invalid_argument(
                        "Only the first message may contain write_object_spec",
                    ));
                }
                if (message.flush || message.state_lookup) && !message.finish_write {
                    return Err(unsupported(
                        "non-resumable flush/state_lookup; use StartResumableWrite and upload_id",
                        caller,
                    ));
                }
                if message.object_checksums.is_some() {
                    if !first && !message.finish_write {
                        return Err(Status::invalid_argument(
                            "object_checksums is allowed only in the first or final message",
                        ));
                    }
                    expected = merge_hashes(expected, hashes(message.object_checksums.as_ref())?)?;
                }
                let start = u64::try_from(message.write_offset).map_err(|_| {
                    Status::out_of_range(
                        "write_offset must be nonnegative; use 0 for the first message",
                    )
                })?;
                if start > offset || (first && start != 0) {
                    return Err(Status::out_of_range(
                        "write_offset exceeds received bytes; start at 0 and continue at the received byte count",
                    ));
                }
                let data = match message.data.take() {
                    Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
                    None => pb::ChecksummedData::default(),
                };
                if data
                    .crc32c
                    .is_some_and(|crc| crc != crc32c::crc32c(&data.content))
                {
                    return Err(Status::invalid_argument(
                        "checksummed_data.crc32c mismatch; resend the correct data and checksum",
                    ));
                }
                let discard = (offset - start).min(data.content.len() as u64) as usize;
                let added = &data.content[discard..];
                offset = offset
                    .checked_add(added.len() as u64)
                    .ok_or_else(|| Status::out_of_range("Object size overflow"))?;
                if offset > self.max_object_bytes {
                    return Err(Status::resource_exhausted(
                        "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
                    ));
                }
                if total.is_some_and(|total| {
                    offset > total || (message.finish_write && offset != total)
                }) {
                    return Err(Status::out_of_range(
                        "Written bytes do not match object_size; start a new upload with the correct size",
                    ));
                }
                hasher.update(added);
                for chunk in added.chunks(64 * 1024) {
                    tx.send(Ok(UploadChunk::Data(Bytes::copy_from_slice(chunk))))
                        .await
                        .map_err(|_| Status::cancelled("Upload consumer stopped"))?;
                }
                if message.finish_write {
                    break;
                }
                message = stream.next().await.transpose()?.ok_or_else(|| {
                    Status::invalid_argument(
                        "Non-resumable write requires finish_write=true on its final message",
                    )
                })?;
                first = false;
            }
            if require_eof && stream.next().await.transpose()?.is_some() {
                return Err(Status::invalid_argument(
                    "A message followed finish_write; finish_write must be the final message",
                ));
            }
            let actual = hasher.finish();
            if expected.crc32c.is_some_and(|crc| crc != actual.crc32c)
                || expected.md5.is_some_and(|md5| md5 != actual.md5)
            {
                return Err(Status::invalid_argument(
                    "object_checksums mismatch; send checksums for the complete object",
                ));
            }
            tx.send(Ok(UploadChunk::End))
                .await
                .map_err(|_| Status::cancelled("Upload consumer stopped"))
        };
        let write = self.state.catalog.put_object_prechecked(
            &bucket,
            &name,
            options.content_type.clone(),
            options.expected,
            options.conditions,
            self.max_object_bytes,
            rx,
            permit,
            options.metadata.clone(),
            options.access.clone(),
        );
        let (sent, written) = tokio::join!(producer, write);
        let record = match written {
            Ok(record) => {
                sent?;
                record
            }
            Err(error) => {
                if matches!(error, ObjectError::BodyAborted(_)) {
                    sent?;
                }
                return Err(object_status(error));
            }
        };
        let resource = convert::object(&record)?;
        let result = pb::bidi_write_object_response::WriteStatus::Resource(resource);
        if let Some(tx) = responses {
            tx.send(Ok(pb::BidiWriteObjectResponse {
                write_status: Some(result.clone()),
                ..Default::default()
            }))
            .await
            .map_err(|_| Status::cancelled("Response stream closed"))?;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{Auth, Registry},
        clock::SystemClock,
        storage::Catalog,
        test_support::TestDir,
    };
    use std::sync::{Arc, atomic::AtomicBool};

    async fn setup(strict: bool) -> (TestDir, Storage) {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let definition = json::parse_insert(
            br#"{"name":"grpc-io","versioning":{"enabled":true}}"#,
            "project",
            std::time::SystemTime::now(),
        )
        .unwrap();
        catalog.insert_bucket(definition).await.unwrap();
        let auth = if strict {
            Auth::strict(Registry::parse(br#"{"identities":[{"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project":["roles/storage.admin"]}},{"id":"reader@example.invalid","tokens":["reader"],"project_roles":{"project":["roles/storage.objectViewer"]}}]}"#).unwrap())
        } else {
            Auth::permissive()
        };
        let storage = Storage {
            state: Arc::new(crate::server::State {
                ready: AtomicBool::new(true),
                clock: Arc::new(SystemClock),
                catalog,
                api_base_url: "http://localhost:4443".into(),
                default_location: "US".into(),
                auth,
            }),
            max_object_bytes: 1024 * 1024,
        };
        (dir, storage)
    }

    fn spec(name: &str) -> pb::WriteObjectSpec {
        pb::WriteObjectSpec {
            resource: Some(pb::Object {
                bucket: "projects/_/buckets/grpc-io".into(),
                name: name.into(),
                metadata: [("key".into(), "value".into())].into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn message(name: &str, data: &[u8]) -> pb::BidiWriteObjectRequest {
        pb::BidiWriteObjectRequest {
            first_message: Some(
                pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec(name)),
            ),
            finish_write: true,
            data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
                pb::ChecksummedData {
                    content: data.to_vec(),
                    crc32c: Some(crc32c::crc32c(data)),
                },
            )),
            ..Default::default()
        }
    }

    async fn write(
        storage: &Storage,
        messages: Vec<pb::BidiWriteObjectRequest>,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter(messages.into_iter().map(Ok)),
                None,
                None,
                true,
            )
            .await
    }

    #[test]
    fn grpc_io_range_and_masks() {
        assert_eq!(
            json_status(json::JsonError::new(
                hyper::StatusCode::NOT_MODIFIED,
                "notModified",
                "Object condition matched"
            ))
            .code(),
            tonic::Code::FailedPrecondition
        );
        let unsupported = json_status(json::JsonError::new(
            hyper::StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            "Unsupported object option",
        ));
        assert_eq!(unsupported.code(), tonic::Code::Unimplemented);
        assert!(!unsupported.details().is_empty());
        assert_eq!(read_range(15, -5, 3).unwrap(), (10, 3));
        assert_eq!(read_range(15, i64::MIN, 0).unwrap(), (0, 15));
        assert_eq!(read_range(15, 15, 0).unwrap(), (15, 0));
        assert_eq!(
            read_range(15, 16, 0).unwrap_err().code(),
            tonic::Code::OutOfRange
        );
        assert_eq!(
            read_range(15, 0, -1).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        let object = mask_object(
            pb::Object {
                name: "a".into(),
                size: 12,
                ..Default::default()
            },
            Some(&prost_types::FieldMask {
                paths: vec!["name".into()],
            }),
        )
        .unwrap();
        assert_eq!(object.name, "a");
        assert_eq!(object.size, 0);
        assert_eq!(
            mask_object(
                pb::Object::default(),
                Some(&prost_types::FieldMask {
                    paths: vec!["acl".into()]
                })
            )
            .unwrap_err()
            .code(),
            tonic::Code::Unimplemented
        );
    }

    #[tokio::test]
    async fn grpc_io_write_spec_storage_class() {
        let (_dir, storage) = setup(false).await;
        let mut request = message("cold", b"x");
        let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
            &mut request.first_message
        else {
            unreachable!()
        };
        spec.resource.as_mut().unwrap().storage_class = "COLDLINE".into();
        let pb::bidi_write_object_response::WriteStatus::Resource(object) =
            write(&storage, vec![request.clone()]).await.unwrap()
        else {
            panic!("write did not finish");
        };
        assert_eq!(object.storage_class, "COLDLINE");
        let stored = storage
            .state
            .catalog
            .get_object("grpc-io", "cold", None, Default::default())
            .await
            .unwrap();
        assert_eq!(stored.storage_class, "COLDLINE");
        for (class, code) in [
            ("GLACIER", tonic::Code::InvalidArgument),
            ("REGIONAL", tonic::Code::Unimplemented),
        ] {
            let mut bad = request.clone();
            let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
                &mut bad.first_message
            else {
                unreachable!()
            };
            spec.resource.as_mut().unwrap().storage_class = class.into();
            assert_eq!(
                write(&storage, vec![bad]).await.unwrap_err().code(),
                code,
                "{class}"
            );
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_resumes_durable_offsets_and_queries_completion() {
        let (dir, storage) = setup(false).await;
        let upload = storage
            .start_resumable_write(
                Request::new(pb::StartResumableWriteRequest {
                    write_object_spec: Some(spec("resume")),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner()
            .upload_id;
        let mut first = message("ignored", b"abc");
        first.finish_write = false;
        first.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload.clone(),
        ));
        assert!(matches!(
            write(&storage, vec![first.clone()]).await.unwrap(),
            pb::bidi_write_object_response::WriteStatus::PersistedSize(3)
        ));
        let query = || {
            Request::new(pb::QueryWriteStatusRequest {
                upload_id: upload.clone(),
                ..Default::default()
            })
        };
        assert!(matches!(
            storage
                .query_write_status(query(), None)
                .await
                .unwrap()
                .into_inner()
                .write_status,
            Some(pb::query_write_status_response::WriteStatus::PersistedSize(
                3
            ))
        ));
        storage.state.catalog.close().await.unwrap();
        let mut storage = storage;
        Arc::get_mut(&mut storage.state).unwrap().catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            write(&storage, vec![first]).await.unwrap_err().code(),
            tonic::Code::OutOfRange
        );
        let mut final_message = message("ignored", b"def");
        final_message.write_offset = 3;
        final_message.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload.clone(),
        ));
        let result = write(&storage, vec![final_message]).await.unwrap();
        let pb::bidi_write_object_response::WriteStatus::Resource(object) = result else {
            panic!("not complete")
        };
        assert_eq!(object.size, 6);
        assert_eq!(
            object.checksums.unwrap().crc32c,
            Some(crc32c::crc32c(b"abcdef"))
        );
        assert!(matches!(
            storage
                .query_write_status(
                    Request::new(pb::QueryWriteStatusRequest {
                        upload_id: upload,
                        ..Default::default()
                    }),
                    None
                )
                .await
                .unwrap()
                .into_inner()
                .write_status,
            Some(pb::query_write_status_response::WriteStatus::Resource(_))
        ));
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_checksums_conditions_and_generation_preserve_data() {
        let (_dir, storage) = setup(false).await;
        let first = write(&storage, vec![message("object", b"original")])
            .await
            .unwrap();
        let pb::bidi_write_object_response::WriteStatus::Resource(first) = first else {
            panic!("not complete")
        };
        for checksum in [
            pb::ObjectChecksums {
                crc32c: Some(1),
                ..Default::default()
            },
            pb::ObjectChecksums {
                md5_hash: vec![0; 16],
                ..Default::default()
            },
        ] {
            let mut bad = message("object", b"bad");
            bad.object_checksums = Some(checksum);
            assert_eq!(
                write(&storage, vec![bad]).await.unwrap_err().code(),
                tonic::Code::InvalidArgument
            );
            assert_eq!(
                storage
                    .state
                    .catalog
                    .get_object("grpc-io", "object", None, ObjectConditions::default())
                    .await
                    .unwrap()
                    .generation,
                first.generation
            );
        }
        let mut absent = spec("atomic");
        absent.if_generation_match = Some(0);
        let upload = storage
            .start_resumable_write(
                Request::new(pb::StartResumableWriteRequest {
                    write_object_spec: Some(absent),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner()
            .upload_id;
        write(&storage, vec![message("atomic", b"winner")])
            .await
            .unwrap();
        let mut losing = message("ignored", b"loser");
        losing.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload,
        ));
        assert_eq!(
            write(&storage, vec![losing]).await.unwrap_err().code(),
            tonic::Code::FailedPrecondition
        );
        write(&storage, vec![message("object", b"new")])
            .await
            .unwrap();
        let get = pb::GetObjectRequest {
            bucket: "projects/_/buckets/grpc-io".into(),
            object: "object".into(),
            generation: first.generation,
            read_mask: Some(prost_types::FieldMask {
                paths: vec!["generation".into(), "size".into()],
            }),
            ..Default::default()
        };
        let selected = storage
            .get_object(Request::new(get.clone()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(selected.generation, first.generation);
        assert_eq!(selected.size, 8);
        assert!(selected.name.is_empty());
        let mut reader = storage
            .read_object(
                Request::new(pb::ReadObjectRequest {
                    bucket: get.bucket.clone(),
                    object: get.object.clone(),
                    generation: first.generation,
                    read_offset: -5,
                    read_limit: 3,
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let response = reader.next().await.unwrap().unwrap();
        let data = response.checksummed_data.unwrap();
        assert_eq!(data.content, b"gin");
        assert_eq!(data.crc32c, Some(crc32c::crc32c(b"gin")));
        assert_eq!(
            response.object_checksums.unwrap().crc32c,
            Some(crc32c::crc32c(b"original"))
        );
        storage
            .delete_object(
                Request::new(pb::DeleteObjectRequest {
                    bucket: get.bucket.clone(),
                    object: get.object.clone(),
                    generation: first.generation,
                    if_generation_match: Some(first.generation),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(
            storage
                .get_object(Request::new(get), None)
                .await
                .unwrap_err()
                .code(),
            tonic::Code::NotFound
        );
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_strict_uses_shared_permissions_and_rejects_unsupported_values() {
        let (_dir, storage) = setup(true).await;
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("authorization", "Bearer admin".parse().unwrap());
        let admin = storage.authenticate(&request).unwrap();
        storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter([Ok(message("private", b"private"))]),
                admin,
                None,
                true,
            )
            .await
            .unwrap();
        request
            .metadata_mut()
            .insert("authorization", "Bearer reader".parse().unwrap());
        let reader = storage.authenticate(&request).unwrap();
        assert_eq!(
            storage
                .receive_write(
                    &mut tonic::codegen::tokio_stream::iter([Ok(message("private", b"denied"))]),
                    reader,
                    None,
                    true
                )
                .await
                .unwrap_err()
                .code(),
            tonic::Code::PermissionDenied
        );
        let get = pb::GetObjectRequest {
            bucket: "projects/_/buckets/grpc-io".into(),
            object: "private".into(),
            ..Default::default()
        };
        assert_eq!(
            storage
                .get_object(Request::new(get.clone()), reader)
                .await
                .unwrap()
                .into_inner()
                .size,
            7
        );
        assert!(
            storage
                .get_object(Request::new(get), Some(Caller::Anonymous))
                .await
                .is_err()
        );
        let mut unsupported_spec = spec("kms");
        unsupported_spec.resource.as_mut().unwrap().kms_key = "key".into();
        assert_eq!(
            storage
                .write_spec(unsupported_spec, None, admin)
                .await
                .unwrap_err()
                .code(),
            tonic::Code::Unimplemented
        );
        storage.state.catalog.close().await.unwrap();
    }

    async fn start(storage: &Storage, name: &str, caller: Option<Caller<'_>>) -> String {
        storage
            .start_resumable_write(
                Request::new(pb::StartResumableWriteRequest {
                    write_object_spec: Some(spec(name)),
                    ..Default::default()
                }),
                caller,
            )
            .await
            .unwrap()
            .into_inner()
            .upload_id
    }

    async fn cancel(
        storage: &Storage,
        upload_id: &str,
        caller: Option<Caller<'_>>,
    ) -> Result<(), tonic::Code> {
        storage
            .cancel_resumable_write(
                Request::new(pb::CancelResumableWriteRequest {
                    upload_id: upload_id.into(),
                }),
                caller,
            )
            .await
            .map(|_| ())
            .map_err(|status| status.code())
    }

    async fn query(
        storage: &Storage,
        upload_id: &str,
        caller: Option<Caller<'_>>,
    ) -> Result<pb::query_write_status_response::WriteStatus, tonic::Code> {
        storage
            .query_write_status(
                Request::new(pb::QueryWriteStatusRequest {
                    upload_id: upload_id.into(),
                    ..Default::default()
                }),
                caller,
            )
            .await
            .map(|response| response.into_inner().write_status.unwrap())
            .map_err(|status| status.code())
    }

    fn resume(
        upload_id: &str,
        offset: i64,
        data: &[u8],
        finish: bool,
    ) -> pb::BidiWriteObjectRequest {
        let mut next = message("ignored", data);
        next.write_offset = offset;
        next.finish_write = finish;
        next.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload_id.into(),
        ));
        next
    }

    async fn missing(storage: &Storage, name: &str) -> bool {
        storage
            .state
            .catalog
            .get_object("grpc-io", name, None, ObjectConditions::default())
            .await
            .is_err_and(|error| error == ObjectError::ObjectNotFound)
    }

    #[tokio::test]
    async fn grpc_io_cancel_discards_sessions_and_staged_bytes() {
        let (dir, storage) = setup(false).await;
        let fresh = start(&storage, "fresh", None).await;
        assert_eq!(cancel(&storage, &fresh, None).await, Ok(()));
        assert_eq!(
            query(&storage, &fresh, None).await,
            Err(tonic::Code::NotFound)
        );
        for finish in [false, true] {
            assert_eq!(
                write(&storage, vec![resume(&fresh, 0, b"abc", finish)])
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::NotFound
            );
        }
        // Repeating a successful cancellation reports the session as gone rather than
        // succeeding again, so callers cannot mistake it for a still-live upload.
        assert_eq!(
            cancel(&storage, &fresh, None).await,
            Err(tonic::Code::NotFound)
        );

        let partial = start(&storage, "partial", None).await;
        assert!(matches!(
            write(&storage, vec![resume(&partial, 0, b"abc", false)])
                .await
                .unwrap(),
            pb::bidi_write_object_response::WriteStatus::PersistedSize(3)
        ));
        let (bucket, id) = parse_upload_id(&partial).unwrap();
        let blob = storage
            .state
            .catalog
            .resumable_session(id, bucket)
            .await
            .unwrap()
            .blob_id;
        let staged = dir.0.join("staging").join(&blob);
        assert!(staged.exists());
        assert_eq!(cancel(&storage, &partial, None).await, Ok(()));
        assert!(!staged.exists());
        assert!(!dir.0.join("blobs").join(&blob[..2]).join(&blob).exists());
        assert_eq!(
            write(&storage, vec![resume(&partial, 3, b"def", true)])
                .await
                .unwrap_err()
                .code(),
            tonic::Code::NotFound
        );
        for name in ["fresh", "partial"] {
            assert!(missing(&storage, name).await);
        }

        let done = start(&storage, "done", None).await;
        write(&storage, vec![resume(&done, 0, b"done", true)])
            .await
            .unwrap();
        assert_eq!(
            cancel(&storage, &done, None).await,
            Err(tonic::Code::FailedPrecondition)
        );
        assert!(matches!(
            query(&storage, &done, None).await,
            Ok(pb::query_write_status_response::WriteStatus::Resource(_))
        ));
        assert!(!missing(&storage, "done").await);

        let unknown = format!("grpc-io/{}", "0".repeat(64));
        for id in [
            unknown.as_str(),
            "",
            "grpc-io",
            "grpc-io/not-hex",
            "/0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(cancel(&storage, id, None).await, Err(tonic::Code::NotFound));
        }
        // An upload_id is scoped to its bucket; reusing it under another bucket finds nothing.
        let scoped = start(&storage, "scoped", None).await;
        let (_, id) = parse_upload_id(&scoped).unwrap();
        assert_eq!(
            cancel(&storage, &format!("other-bucket/{id}"), None).await,
            Err(tonic::Code::NotFound)
        );
        assert!(query(&storage, &scoped, None).await.is_ok());

        // An in-flight writer holds the session lease; cancellation reports a retryable
        // conflict and leaves the session intact instead of racing the writer.
        let lease = storage
            .state
            .catalog
            .lock_resumable_session(id, "grpc-io")
            .await
            .unwrap();
        assert_eq!(
            cancel(&storage, &scoped, None).await,
            Err(tonic::Code::Unavailable)
        );
        drop(lease);
        assert_eq!(cancel(&storage, &scoped, None).await, Ok(()));
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_strict_cancel_requires_create_permission() {
        let (_dir, storage) = setup(true).await;
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("authorization", "Bearer admin".parse().unwrap());
        let admin = storage.authenticate(&request).unwrap();
        request
            .metadata_mut()
            .insert("authorization", "Bearer reader".parse().unwrap());
        let reader = storage.authenticate(&request).unwrap();
        let upload = start(&storage, "strict", admin).await;
        storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter([Ok(resume(&upload, 0, b"abc", false))]),
                admin,
                None,
                true,
            )
            .await
            .unwrap();
        assert_eq!(
            cancel(&storage, &upload, reader).await,
            Err(tonic::Code::PermissionDenied)
        );
        assert_eq!(
            cancel(&storage, &upload, Some(Caller::Anonymous)).await,
            Err(tonic::Code::Unauthenticated)
        );
        assert!(matches!(
            query(&storage, &upload, admin).await,
            Ok(pb::query_write_status_response::WriteStatus::PersistedSize(
                3
            ))
        ));
        assert_eq!(cancel(&storage, &upload, admin).await, Ok(()));
        assert_eq!(
            query(&storage, &upload, admin).await,
            Err(tonic::Code::NotFound)
        );
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_resumable_checksums_survive_reconnect_and_failed_completion() {
        let (dir, mut storage) = setup(false).await;
        let mut hasher = crate::hashing::Hasher::default();
        hasher.update(b"abcdef");
        let correct = hasher.finish();
        for (name, checksum) in [
            (
                "resume-crc",
                pb::ObjectChecksums {
                    crc32c: Some(correct.crc32c),
                    ..Default::default()
                },
            ),
            (
                "resume-md5",
                pb::ObjectChecksums {
                    md5_hash: correct.md5.to_vec(),
                    ..Default::default()
                },
            ),
        ] {
            let upload = storage
                .start_resumable_write(
                    Request::new(pb::StartResumableWriteRequest {
                        write_object_spec: Some(spec(name)),
                        object_checksums: Some(checksum.clone()),
                        ..Default::default()
                    }),
                    None,
                )
                .await
                .unwrap()
                .into_inner()
                .upload_id;
            let mut prefix = message("ignored", b"abc");
            prefix.finish_write = false;
            prefix.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
                upload.clone(),
            ));
            let mut rejected = prefix.clone();
            rejected.object_checksums = Some(checksum);
            assert_eq!(
                write(&storage, vec![rejected]).await.unwrap_err().code(),
                tonic::Code::Unimplemented
            );
            assert!(matches!(
                storage
                    .query_write_status(
                        Request::new(pb::QueryWriteStatusRequest {
                            upload_id: upload.clone(),
                            ..Default::default()
                        }),
                        None
                    )
                    .await
                    .unwrap()
                    .into_inner()
                    .write_status,
                Some(pb::query_write_status_response::WriteStatus::PersistedSize(
                    0
                ))
            ));
            write(&storage, vec![prefix]).await.unwrap();
            storage.state.catalog.close().await.unwrap();
            Arc::get_mut(&mut storage.state).unwrap().catalog =
                Catalog::open(&dir.0).await.unwrap();
            let mut bad = message("ignored", b"xxx");
            bad.write_offset = 3;
            bad.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
                upload.clone(),
            ));
            assert_eq!(
                write(&storage, vec![bad.clone()]).await.unwrap_err().code(),
                tonic::Code::InvalidArgument
            );
            assert_eq!(
                storage
                    .state
                    .catalog
                    .get_object("grpc-io", name, None, ObjectConditions::default())
                    .await
                    .unwrap_err(),
                ObjectError::ObjectNotFound
            );
            assert_eq!(
                storage
                    .query_write_status(
                        Request::new(pb::QueryWriteStatusRequest {
                            upload_id: upload,
                            ..Default::default()
                        }),
                        None
                    )
                    .await
                    .unwrap_err()
                    .code(),
                tonic::Code::NotFound
            );
            assert_eq!(
                write(&storage, vec![bad]).await.unwrap_err().code(),
                tonic::Code::NotFound
            );
        }
        let upload = storage
            .start_resumable_write(
                Request::new(pb::StartResumableWriteRequest {
                    write_object_spec: Some(spec("final-crc")),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner()
            .upload_id;
        let mut prefix = message("ignored", b"abc");
        prefix.finish_write = false;
        prefix.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload.clone(),
        ));
        write(&storage, vec![prefix]).await.unwrap();
        let mut bad = message("ignored", b"def");
        bad.write_offset = 3;
        bad.first_message = Some(pb::bidi_write_object_request::FirstMessage::UploadId(
            upload,
        ));
        bad.object_checksums = Some(pb::ObjectChecksums {
            crc32c: Some(1),
            ..Default::default()
        });
        assert_eq!(
            write(&storage, vec![bad]).await.unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            storage
                .state
                .catalog
                .get_object("grpc-io", "final-crc", None, ObjectConditions::default())
                .await
                .unwrap_err(),
            ObjectError::ObjectNotFound
        );
        storage.state.catalog.close().await.unwrap();
    }
}
