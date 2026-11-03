use super::{
    ResponseStream, Storage, access_status, convert, pb,
    status::{read_failure, unsupported_for},
};
use crate::{
    auth::{Caller, OBJECTS_CREATE, OBJECTS_DELETE, OBJECTS_GET, OBJECTS_LIST},
    grpc::{
        upload::{self, EndMarker, Intake, IntakeError, IntakePiece, Resend},
        validate::{self, request_conditions},
    },
    json,
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget, UploadChunk,
    },
    storage::{
        Finality, ObjectUpload, ResumableChunk, ResumableError, ResumableOptions, SessionState,
    },
};
use prost::Message;
use tokio::sync::{mpsc, watch};
use tonic::codegen::tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};

/// `?` would use JSON's unsupported-setting description instead of the v2 metadata wording.
pub(super) fn object_status(error: ObjectError) -> Status {
    if matches!(error, ObjectError::UnsupportedMetadata(_)) {
        return unsupported_for(&error.to_string(), None);
    }
    error.into()
}

/// [`object_status`] for an RPC that names one object: Cloud Storage repeats `<bucket>/<object>`
/// in its not-found message over gRPC.
pub(super) fn named_object_status<'a>(
    bucket: &'a str,
    name: &'a str,
) -> impl Fn(ObjectError) -> Status + 'a {
    move |error| {
        if matches!(error, ObjectError::UnsupportedMetadata(_)) {
            return object_status(error);
        }
        json::JsonError::from(error)
            .naming_object(bucket, name)
            .into()
    }
}

fn session_status(error: ResumableError) -> Status {
    match error {
        ResumableError::NotFound | ResumableError::Gone | ResumableError::Cancelled => {
            Status::not_found("Upload session not found; start a new resumable write")
        }
        ResumableError::Invalid(message) => Status::invalid_argument(message),
        ResumableError::InvalidContentRange => Status::invalid_argument(error.to_string()),
        ResumableError::Object(error) => object_status(error),
    }
}

pub(super) fn common(params: Option<&pb::CommonObjectRequestParams>) -> Result<(), Status> {
    validate::common(
        params,
        "customer encryption; omit common_object_request_params",
    )
}

pub(super) fn conditions(conditions: ObjectConditions) -> Result<ObjectConditions, Status> {
    validate::lenient_conditions(
        conditions,
        "Preconditions must be nonnegative; use if_generation_match=0 to create only",
    )
}

fn generation(value: i64) -> Result<Option<i64>, Status> {
    validate::generation(
        value,
        "generation must be positive, or 0 to select the live object",
    )
}

pub(super) fn hashes(value: Option<&pb::ObjectChecksums>) -> Result<ExpectedHashes, Status> {
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
    validate::merge_hashes(
        first,
        next,
        "Conflicting object_checksums; use the same digest in each message",
    )
}

/// The object fields that a default read mask omits, selected by an explicit mask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct OwnerFields {
    acl: bool,
    owner: bool,
}

impl OwnerFields {
    /// Every owner-related field, which Cloud Storage returns on the resource of a write or
    /// update (observed 2026-11-01) though a read returns them only when its mask asks.
    const ALL: Self = Self {
        acl: true,
        owner: true,
    };

    /// `*` selects every field; other paths name top-level fields, optionally under `prefix`
    /// (ReadObject nests the object in `metadata`, where `metadata` alone selects all of it).
    fn selected(mask: Option<&prost_types::FieldMask>, prefix: Option<&str>) -> Self {
        let Some(mask) = mask else {
            return Self::default();
        };
        let path = |field: &str| {
            mask.paths.iter().any(|path| {
                path == "*"
                    || match prefix {
                        None => path == field,
                        Some(prefix) => {
                            path == prefix
                                || path
                                    .strip_prefix(prefix)
                                    .and_then(|rest| rest.strip_prefix('.'))
                                    .is_some_and(|rest| rest == field)
                        }
                    }
            })
        };
        Self {
            acl: path("acl"),
            owner: path("owner"),
        }
    }

    fn any(self) -> bool {
        self.acl || self.owner
    }
}

/// Keeps the top-level `Object` fields a read mask names. Every field is accepted; fields the
/// local object cannot hold (KMS key, contexts, restore token of a live object) stay empty.
/// ACL and owner are filled beforehand by [`Storage::add_owner_fields`].
fn mask_object(
    mut object: pb::Object,
    mask: Option<&prost_types::FieldMask>,
) -> Result<pb::Object, Status> {
    let Some(mask) = mask.filter(|m| !m.paths.is_empty() && !m.paths.iter().any(|p| p == "*"))
    else {
        return Ok(object);
    };
    // The Java SDK lists with `checksums.crc32c` and `checksums.md5_hash`; a sub-path keeps only
    // that digest, as protobuf field-mask semantics require.
    let digest = |name: &str| {
        mask.paths
            .iter()
            .any(|p| p == "checksums" || p.strip_prefix("checksums.") == Some(name))
    };
    let kept_checksums = object.checksums.take().map(|mut sums| {
        if !digest("crc32c") {
            sums.crc32c = None;
        }
        if !digest("md5_hash") {
            sums.md5_hash = Default::default();
        }
        sums
    });
    macro_rules! fields { ($($field:ident),+ $(,)?) => {{
        for path in &mask.paths {
            if !matches!(path.as_str(), $(stringify!($field))|+ | "checksums.crc32c" | "checksums.md5_hash") {
                // A name that is no Object field is a client error. A sub-path of a real field
                // (`owner.entity`) is only unsupported here: the recording cannot tell whether
                // Cloud Storage accepts it, so it stays an error instead of a false success.
                let root = path.split('.').next().unwrap_or_default();
                return Err(if matches!(root, $(stringify!($field))|+) {
                    unsupported_for(&format!("read_mask path {path}; request a supported top-level object field"), None)
                } else {
                    Status::invalid_argument(format!("There is no field for this resource named '{path}'."))
                });
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
    if digest("crc32c") || digest("md5_hash") {
        object.checksums = kept_checksums;
    }
    Ok(object)
}

/// Rejects a data message whose CRC32C differs from its content, with the wording and hex form
/// Cloud Storage uses over gRPC.
pub(super) fn verify_crc32c(data: &pb::ChecksummedData) -> Result<(), Status> {
    upload::verify_crc32c(&data.content, data.crc32c).map_err(intake_status)
}

fn intake_status(error: IntakeError) -> Status {
    match error {
        IntakeError::Offset => Status::out_of_range(
            "write_offset exceeds received bytes; start at 0 and continue at the received byte count",
        ),
        IntakeError::ChunkCrc { computed, client } => Status::invalid_argument(format!(
            "Checksummed data message crc32c mismatch: server computed: '{computed:08x}', client provided '{client:08x}'."
        )),
        IntakeError::Overflow => Status::out_of_range("Object size overflow"),
        IntakeError::TooLarge => Status::resource_exhausted(
            "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
        ),
        IntakeError::SizeMismatch => Status::out_of_range(
            "Written bytes do not match object_size; start a new upload with the correct size",
        ),
        IntakeError::ObjectChecksum => Status::invalid_argument(
            "object_checksums mismatch; send checksums for the complete object",
        ),
        IntakeError::ConflictingChecksums => Status::invalid_argument(
            "Conflicting object_checksums; use the same digest in each message",
        ),
        IntakeError::ConsumerStopped => Status::cancelled("Upload consumer stopped"),
    }
}

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

fn read_range(size: u64, offset: i64, limit: i64) -> Result<(u64, u64), Status> {
    if limit < 0 {
        return Err(Status::invalid_argument(
            "read_limit must be nonnegative; use 0 to read to the end",
        ));
    }
    validate::resolve_range(size, offset, limit).ok_or_else(|| {
        Status::out_of_range("read_offset exceeds object size; use 0 or a valid byte offset")
    })
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
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let access = self.access(caller, bucket, OBJECTS_DELETE).await?;
        let target = ObjectTarget::new(bucket, name, generation(r.generation)?);
        self.delete_including_appendable(target, request_conditions!(conditions, r)?, &access)
            .await
            .map_err(named_object_status(bucket, name))?;
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
        self.access(caller, bucket, OBJECTS_CREATE).await?;
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
        let mut object = super::append::object(&record, finality)?;
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
    /// Fills ACL and owner on the resource a write or update answers with. A caller that may
    /// write but not read the ACL still gets its result, with both fields empty.
    pub(super) async fn written_object(
        &self,
        record: &ObjectRecord,
        caller: Option<Caller<'_>>,
    ) -> Result<pb::Object, Status> {
        let mut object = convert::object(record)?;
        self.add_written_owner_fields(caller, &record.bucket, &mut object)
            .await?;
        Ok(object)
    }

    pub(super) async fn add_written_owner_fields(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        object: &mut pb::Object,
    ) -> Result<(), Status> {
        match self
            .add_owner_fields(
                caller,
                bucket,
                std::slice::from_mut(object),
                OwnerFields::ALL,
            )
            .await
        {
            Err(status)
                if matches!(
                    status.code(),
                    tonic::Code::PermissionDenied | tonic::Code::Unauthenticated
                ) =>
            {
                Ok(())
            }
            other => other,
        }
    }
    /// Fills the ACL and owner a read mask selects, from the same shared projection as JSON
    /// `projection=full`: under uniform bucket-level access, or for an object whose ACL the
    /// caller may not read (`storage.objects.getIamPolicy`), both stay empty and the read still
    /// succeeds.
    async fn add_owner_fields(
        &self,
        caller: Option<Caller<'_>>,
        bucket: &str,
        objects: &mut [pb::Object],
        fields: OwnerFields,
    ) -> Result<(), Status> {
        if !fields.any() || objects.is_empty() {
            return Ok(());
        }
        let generations = objects
            .iter()
            .map(|object| (object.name.clone(), object.generation))
            .collect();
        let Some(acl) =
            crate::server::readable_object_acls(&self.state, caller, bucket, generations)
                .await
                .map_err(|e| access_status(e, caller))?
        else {
            return Ok(());
        };
        for (((object, entries), owner), visible) in objects
            .iter_mut()
            .zip(acl.objects)
            .zip(acl.owners)
            .zip(acl.visible)
        {
            if !visible {
                continue;
            }
            if fields.acl {
                object.acl = convert::object_acl(object, bucket, &acl.project, &entries);
            }
            if fields.owner {
                object.owner = owner
                    .and_then(|owner| owner.entity())
                    .map(|entity| pb::Owner {
                        entity,
                        entity_id: String::new(),
                    });
            }
        }
        Ok(())
    }
    pub(super) async fn read_object(
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
        let mut object = super::append::object(&record, finality)?;
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
    pub(super) async fn write_object(
        &self,
        request: Request<Streaming<pb::WriteObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::WriteObjectResponse>, Status> {
        let metadata = request.metadata().clone();
        let mut position = MessagePosition::First;
        let mut stream = request.into_inner().map(|result| {
            result.and_then(|r| {
                let message = pb::BidiWriteObjectRequest {
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
                };
                if position == MessagePosition::First {
                    super::routing::write(&metadata, message.first_message.as_ref())?;
                    position = MessagePosition::Later;
                }
                Ok(message)
            })
        });
        let result = self
            .receive_write(&mut stream, caller, None, EndOfStream::Required)
            .await?;
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
        let metadata = request.metadata().clone();
        let mut stream = request.into_inner();
        let first = stream.message().await?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        super::routing::write(&metadata, first.first_message.as_ref())?;
        let append = super::append::is_append(&first);
        let strict = caller.is_some();
        let identity = caller.and_then(|c| match c {
            Caller::Principal(i) => Some(i.clone()),
            Caller::Anonymous => None,
        });
        let storage = Storage {
            state: self.state.clone(),
            max_object_bytes: self.max_object_bytes,
            compat_profile: self.compat_profile,
        };
        let (tx, rx) = mpsc::channel(1);
        crate::task::spawn_supervised("grpc-write-object", async move {
            let caller = strict.then(|| {
                identity
                    .as_ref()
                    .map_or(Caller::Anonymous, Caller::Principal)
            });
            let result = if append {
                let mut stream = stream;
                storage
                    .receive_append(first, &mut stream, caller, &tx)
                    .await
            } else {
                let mut input = tonic::codegen::tokio_stream::once(Ok(first)).chain(stream);
                storage
                    .receive_write(&mut input, caller, Some(&tx), EndOfStream::NotChecked)
                    .await
                    .map(|_| ())
            };
            // Terminal stream errors bypass the RPC wrapper's anonymous mapping.
            if let Err(error) = result {
                let _ = tx.send(Err(super::status::anonymous(error, caller))).await;
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
                .map_err(|error| super::status::glob_status(&error))?,
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
            .map_err(super::status::list_query_status)?;
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
        let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
        let session = self
            .state
            .catalog
            .resumable_session(id, bucket)
            .await
            .map_err(session_status)?;
        let allowed = self
            .state
            .catalog
            .has_bucket_permission(bucket, &access)
            .await
            .map_err(ObjectError::from)
            .map_err(object_status)?;
        if !allowed {
            return Err(object_status(ObjectError::Forbidden));
        }
        let status = if session.state == SessionState::Complete {
            pb::query_write_status_response::WriteStatus::Resource(decode_completion(
                session.completion.as_ref(),
            )?)
        } else {
            pb::query_write_status_response::WriteStatus::PersistedSize(
                crate::grpc::convert::signed(session.offset, "upload session offset")?,
            )
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
    pub(super) async fn write_spec(
        &self,
        mut spec: pb::WriteObjectSpec,
        checksums: Option<&pb::ObjectChecksums>,
        caller: Option<Caller<'_>>,
    ) -> Result<(String, String, ResumableOptions, Option<u64>), Status> {
        if spec.appendable == Some(true) || !spec.predefined_acl.is_empty() {
            return Err(unsupported_for(
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
        let (bucket, name) = self.object_name(&resource.bucket, &resource.name)?;
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = self.access(caller, &bucket, OBJECTS_CREATE).await?;
        let content_type = if resource.content_type.is_empty() {
            "application/octet-stream".to_owned()
        } else {
            std::mem::take(&mut resource.content_type)
        };
        let mut metadata = super::object_update::for_write(&resource);
        metadata.replace = false;
        let storage_class = std::mem::take(&mut resource.storage_class);
        resource.content_encoding.clear();
        resource.content_disposition.clear();
        resource.content_language.clear();
        resource.cache_control.clear();
        resource.metadata.clear();
        resource.event_based_hold = None;
        resource.custom_time = None;
        resource.temporary_hold = false;
        resource.name.clear();
        resource.bucket.clear();
        let expected = merge_hashes(hashes(resource.checksums.as_ref())?, hashes(checksums)?)?;
        resource.checksums = None;
        if resource != pb::Object::default() {
            return Err(unsupported_for(
                "write_object_spec.resource fields; use content_type, fixed/custom metadata and holds",
                caller,
            ));
        }
        metadata.content_type = Some(Some(content_type.clone()));
        metadata.storage_class = (!storage_class.is_empty())
            .then(|| crate::object::validate_object_storage_class(&storage_class))
            .transpose()
            .map_err(super::object_update::input_error)?;
        super::object_update::validate(&metadata)?;
        let conditions = request_conditions!(conditions, spec)?;
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
        responses: Option<&Responses>,
        end: EndOfStream,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        use pb::bidi_write_object_request::FirstMessage;
        let mut message = stream.next().await.transpose()?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        common(message.common_object_request_params.as_ref())?;
        let session = match message.first_message.take() {
            Some(FirstMessage::WriteObjectSpec(spec)) => {
                let write = SingleWrite {
                    spec,
                    caller,
                    responses,
                    end,
                };
                return self.receive_single_write(stream, message, write).await;
            }
            Some(FirstMessage::UploadId(upload)) => {
                let (bucket, id) = parse_upload_id(&upload)?;
                ResumableTarget {
                    bucket: bucket.to_owned(),
                    id: id.to_owned(),
                }
            }
            Some(FirstMessage::AppendObjectSpec(_)) => {
                return Err(unsupported_for("Rapid append", caller));
            }
            None => {
                return Err(Status::invalid_argument(
                    "First message must contain write_object_spec or upload_id",
                ));
            }
        };
        self.receive_resumable_write(stream, message, &session, caller, responses, end)
            .await
    }

    /// Completion keeps the publication snapshot and adds only the caller's ACL/owner view.
    async fn completed_resource(
        &self,
        completion: Option<&serde_json::Value>,
        caller: Option<Caller<'_>>,
        bucket: &str,
    ) -> Result<pb::Object, Status> {
        let mut resource = decode_completion(completion)?;
        self.add_written_owner_fields(caller, bucket, &mut resource)
            .await?;
        Ok(resource)
    }

    async fn receive_resumable_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        mut message: pb::BidiWriteObjectRequest,
        session: &ResumableTarget,
        caller: Option<Caller<'_>>,
        responses: Option<&Responses>,
        end: EndOfStream,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        use pb::bidi_write_object_response::WriteStatus;
        let ResumableTarget { bucket, id } = session;
        let mut position = MessagePosition::First;
        let mut result;
        loop {
            check_resumable_message(&message, caller)?;
            let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
            let lease = self
                .state
                .catalog
                .lock_resumable_session(id, bucket)
                .await
                .map_err(session_status)?;
            if lease.session.state == SessionState::Complete {
                let resource = self
                    .completed_resource(lease.session.completion.as_ref(), caller, bucket)
                    .await?;
                drop(lease);
                let result = WriteStatus::Resource(resource);
                if let Some(tx) = responses {
                    send_status(tx, &result).await?;
                }
                return Ok(result);
            }
            let mut options = lease.session.options.clone();
            options.access = access;
            self.state
                .catalog
                .check_upload(
                    bucket,
                    &lease.session.name,
                    options.conditions,
                    &options.access,
                )
                .await
                .map_err(object_status)?;
            options.expected =
                merge_hashes(options.expected, hashes(message.object_checksums.as_ref())?)?;
            let persisted = Persisted {
                offset: lease.session.offset,
                total: lease.session.total,
            };
            let ResumablePiece {
                offset,
                data,
                written,
                length,
            } = resumable_piece(&mut message, persisted, position, self.max_object_bytes)?;
            if message.finish_write
                && end == EndOfStream::Required
                && stream.next().await.transpose()?.is_some()
            {
                return Err(Status::invalid_argument(
                    "A message followed finish_write; finish_write must be the final message",
                ));
            }
            let (tx, rx) = mpsc::channel(2);
            let (catalog_wait, _wait) = watch::channel(false);
            let chunk = ResumableChunk {
                start: offset,
                length,
                total: persisted.total.or(message.finish_write.then_some(written)),
                finalize: message.finish_write,
                open_end: false,
                options,
                catalog_wait,
            };
            let permit = self
                .state
                .catalog
                .admit_object_upload()
                .map_err(object_status)?;
            let producer = async {
                stage(&tx, &data.content).await?;
                tx.send(Ok(UploadChunk::End))
                    .await
                    .map_err(|_| Status::cancelled("Upload consumer stopped"))
            };
            let write =
                self.state
                    .catalog
                    .write_resumable_session(lease, chunk, rx, permit, |record| {
                        let resource = convert::object(record)?;
                        serde_json::to_value(resource.encode_to_vec())
                            .map_err(|_| ObjectError::InvalidMetadata)
                    });
            let (sent, written_record) = tokio::join!(producer, write);
            let completion = written_record.map_err(session_status)?;
            sent?;
            result = if let Some(value) = completion.as_ref() {
                let resource = self.completed_resource(Some(value), caller, bucket).await?;
                WriteStatus::Resource(resource)
            } else {
                WriteStatus::PersistedSize(crate::grpc::convert::signed(written, "persisted_size")?)
            };
            let reported = if let Some(tx) = responses
                && (message.finish_write || message.flush || message.state_lookup)
            {
                send_status(tx, &result).await?;
                Reported::Yes
            } else {
                Reported::No
            };
            if message.finish_write {
                return Ok(result);
            }
            position = MessagePosition::Later;
            let Some(next) = stream.next().await.transpose()? else {
                if let Some(tx) = responses
                    && reported == Reported::No
                {
                    send_status(tx, &result).await?;
                }
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
        message: pb::BidiWriteObjectRequest,
        write: SingleWrite<'_>,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        let SingleWrite {
            spec,
            caller,
            responses,
            end,
        } = write;
        let (bucket, name, options, total) = self
            .write_spec(spec, message.object_checksums.as_ref(), caller)
            .await?;
        let permit = self
            .state
            .catalog
            .prepare_object_upload(&bucket, &name, options.conditions, &options.access)
            .await
            .map_err(|error| match error {
                // Over WriteObject Cloud Storage words a missing bucket differently from reads.
                ObjectError::BucketNotFound => {
                    Status::not_found(format!("Bucket 'projects/_/buckets/{bucket}' not found"))
                }
                error => object_status(error),
            })?;
        let (tx, rx) = mpsc::channel(2);
        let limits = SingleWriteLimits {
            expected: options.expected,
            total,
            max_object_bytes: self.max_object_bytes,
        };
        let producer = produce_single_write(stream, message, limits, tx, caller, end);
        let write = self.state.catalog.put_object_prechecked(
            ObjectUpload {
                bucket: &bucket,
                name: &name,
                content_type: options.content_type.clone(),
                expected: options.expected,
                conditions: options.conditions,
                max_bytes: self.max_object_bytes,
                metadata: options.metadata.clone(),
                access: options.access.clone(),
            },
            rx,
            permit,
        );
        let (sent, written) = tokio::join!(producer, write);
        let record = upload::completed(sent, written, object_status)?;
        let resource = self.written_object(&record, caller).await?;
        let result = pb::bidi_write_object_response::WriteStatus::Resource(resource);
        if let Some(tx) = responses {
            send_status(tx, &result).await?;
        }
        Ok(result)
    }
}

type Responses = mpsc::Sender<Result<pb::BidiWriteObjectResponse, Status>>;

/// Whether a write must end its request stream at `finish_write`. WriteObject rejects a later
/// message; BidiWriteObject answers at `finish_write` without waiting for the stream to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndOfStream {
    Required,
    NotChecked,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reported {
    Yes,
    No,
}

/// Whether a resumable message is the first one of its stream, which must resume exactly at
/// the persisted offset; later messages may overlap bytes already persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessagePosition {
    First,
    Later,
}

/// The resumable session a write stream continues.
struct ResumableTarget {
    bucket: String,
    id: String,
}

/// What a resumable session had persisted when its lease was taken.
#[derive(Clone, Copy)]
struct Persisted {
    offset: u64,
    total: Option<u64>,
}

struct ResumablePiece {
    offset: u64,
    data: pb::ChecksummedData,
    written: u64,
    length: u64,
}

/// A non-resumable write and where its replies go.
struct SingleWrite<'a> {
    spec: pb::WriteObjectSpec,
    caller: Option<Caller<'a>>,
    responses: Option<&'a Responses>,
    end: EndOfStream,
}

/// The checksum and size limits a non-resumable write's body must satisfy.
#[derive(Clone, Copy)]
struct SingleWriteLimits {
    expected: ExpectedHashes,
    total: Option<u64>,
    max_object_bytes: u64,
}

async fn send_status(
    tx: &Responses,
    status: &pb::bidi_write_object_response::WriteStatus,
) -> Result<(), Status> {
    tx.send(Ok(pb::BidiWriteObjectResponse {
        write_status: Some(status.clone()),
        ..Default::default()
    }))
    .await
    .map_err(|_| Status::cancelled("Response stream closed"))
}

/// Sends `content` to the upload consumer in bounded chunks.
async fn stage(
    tx: &mpsc::Sender<Result<UploadChunk, std::io::Error>>,
    content: &bytes::Bytes,
) -> Result<(), Status> {
    upload::stage(tx, content).await.map_err(intake_status)
}

fn check_resumable_message(
    message: &pb::BidiWriteObjectRequest,
    caller: Option<Caller<'_>>,
) -> Result<(), Status> {
    common(message.common_object_request_params.as_ref())?;
    if message.first_message.is_some() {
        return Err(Status::invalid_argument(
            "Only the first message may contain write_object_spec or upload_id",
        ));
    }
    if message.object_checksums.is_some() && !message.finish_write {
        return Err(unsupported_for(
            "object_checksums before resumable finish; set StartResumableWrite.object_checksums or send them with finish_write=true",
            caller,
        ));
    }
    Ok(())
}

/// Validates one resumable message's offset, data checksum and size against the persisted
/// session, retaining the checked length for the catalog write.
fn resumable_piece(
    message: &mut pb::BidiWriteObjectRequest,
    persisted: Persisted,
    position: MessagePosition,
    max_object_bytes: u64,
) -> Result<ResumablePiece, Status> {
    let offset = u64::try_from(message.write_offset).map_err(|_| {
        Status::out_of_range("write_offset must be nonnegative; query the persisted offset")
    })?;
    if (position == MessagePosition::First && offset != persisted.offset)
        || offset > persisted.offset
    {
        return Err(Status::out_of_range(
            "write_offset does not match persisted data; call QueryWriteStatus and resume at persisted_size",
        ));
    }
    let data = match message.data.take() {
        Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
        None => pb::ChecksummedData::default(),
    };
    verify_crc32c(&data)?;
    let (length, end) = u64::try_from(data.content.len())
        .ok()
        .and_then(|length| offset.checked_add(length).map(|end| (length, end)))
        .filter(|(_, end)| i64::try_from(*end).is_ok())
        .ok_or_else(|| Status::out_of_range("write_offset plus content length overflows"))?;
    let end = end.max(persisted.offset);
    if end > max_object_bytes {
        return Err(Status::resource_exhausted(
            "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
        ));
    }
    if persisted
        .total
        .is_some_and(|total| end > total || (message.finish_write && end != total))
    {
        return Err(Status::out_of_range(
            "Written bytes do not match object_size; start a new upload with the correct size",
        ));
    }
    Ok(ResumablePiece {
        offset,
        data,
        written: end,
        length,
    })
}

/// Streams a non-resumable write's messages to the upload consumer, validating offsets,
/// checksums and sizes, and ends the body only after the whole object checks out.
async fn produce_single_write<
    S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
>(
    stream: &mut S,
    mut message: pb::BidiWriteObjectRequest,
    limits: SingleWriteLimits,
    tx: mpsc::Sender<Result<UploadChunk, std::io::Error>>,
    caller: Option<Caller<'_>>,
    end: EndOfStream,
) -> Result<(), Status> {
    let mut position = MessagePosition::First;
    let mut intake = Intake::new(
        limits.expected,
        limits.max_object_bytes,
        limits.total,
        Resend::DiscardOverlap,
    );
    loop {
        common(message.common_object_request_params.as_ref())?;
        if message.first_message.is_some() {
            return Err(Status::invalid_argument(
                "Only the first message may contain write_object_spec",
            ));
        }
        if (message.flush || message.state_lookup) && !message.finish_write {
            return Err(unsupported_for(
                "non-resumable flush/state_lookup; use StartResumableWrite and upload_id",
                caller,
            ));
        }
        if message.object_checksums.is_some()
            && position == MessagePosition::Later
            && !message.finish_write
        {
            return Err(Status::invalid_argument(
                "object_checksums is allowed only in the first or final message",
            ));
        }
        let checksums = hashes(message.object_checksums.as_ref())?;
        intake.checksums(checksums).map_err(intake_status)?;
        let start = u64::try_from(message.write_offset).map_err(|_| {
            Status::out_of_range("write_offset must be nonnegative; use 0 for the first message")
        })?;
        let data = match message.data.take() {
            Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
            None => pb::ChecksummedData::default(),
        };
        intake
            .accept(
                IntakePiece {
                    offset: start,
                    content: data.content,
                    crc32c: data.crc32c,
                    finish: message.finish_write,
                },
                &tx,
            )
            .await
            .map_err(intake_status)?;
        if message.finish_write {
            break;
        }
        message = upload::next(stream, &tx)
            .await
            .map_err(intake_status)?
            .transpose()?
            .ok_or_else(|| {
                Status::invalid_argument(
                    "Non-resumable write requires finish_write=true on its final message",
                )
            })?;
        position = MessagePosition::Later;
    }
    if end == EndOfStream::Required
        && upload::next(stream, &tx)
            .await
            .map_err(intake_status)?
            .transpose()?
            .is_some()
    {
        return Err(Status::invalid_argument(
            "A message followed finish_write; finish_write must be the final message",
        ));
    }
    intake
        .finish(&tx, EndMarker::Plain)
        .await
        .map_err(intake_status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::test_support::{self as fixtures, assert_code, registry};
    use crate::storage::test_fixtures::{PERMISSIVE, live};
    use crate::{auth::Auth, storage::Catalog, test_support::TestDir};
    use serde_json::json;
    use std::sync::Arc;

    #[test]
    fn resumable_message_end_must_fit_the_wire_offset() {
        let max = i64::MAX.cast_unsigned();
        for (offset, accepted) in [(i64::MAX - 1, true), (i64::MAX, false)] {
            let mut message = pb::BidiWriteObjectRequest {
                write_offset: offset,
                data: Some(pb::bidi_write_object_request::Data::ChecksummedData(
                    pb::ChecksummedData {
                        content: bytes::Bytes::from_static(b"x"),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            };
            let result = resumable_piece(
                &mut message,
                Persisted {
                    offset: offset.cast_unsigned(),
                    total: None,
                },
                MessagePosition::First,
                u64::MAX,
            );
            if accepted {
                assert_eq!(result.unwrap().written, max);
            } else {
                let error = result.err().unwrap();
                assert_eq!(error.code(), tonic::Code::OutOfRange);
                assert_eq!(
                    error.message(),
                    "write_offset plus content length overflows"
                );
            }
        }
    }

    #[test]
    fn object_errors_keep_the_v2_unsupported_metadata_wording() {
        let metadata = object_status(ObjectError::UnsupportedMetadata("test_setting".into()));
        let generic = Status::from(ObjectError::UnsupportedMetadata("test_setting".into()));
        assert_eq!(metadata.code(), tonic::Code::Unimplemented);
        assert_eq!(
            metadata.message(),
            "gcs-mimic does not support gRPC unsupported metadata change: test_setting; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
        );
        assert_ne!(metadata.message(), generic.message());
    }

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
            Auth::strict(registry(&[
                ("admin", "project", "roles/storage.admin"),
                ("reader", "project", "roles/storage.objectViewer"),
                ("user", "project", "roles/storage.objectUser"),
            ]))
        } else {
            Auth::permissive()
        };
        let storage = Storage {
            compat_profile: crate::config::CompatProfile::Native,
            state: Arc::new(crate::server::State::for_test(catalog, auth)),
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
                    content: bytes::Bytes::copy_from_slice(data),
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
        write_as(storage, messages, None).await
    }

    async fn write_as(
        storage: &Storage,
        messages: Vec<pb::BidiWriteObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter(messages.into_iter().map(Ok)),
                caller,
                None,
                EndOfStream::Required,
            )
            .await
    }

    #[tokio::test]
    async fn grpc_io_rejects_unrestorable_names_and_accepts_tabs() {
        let (_dir, storage) = setup(false).await;
        for name in [".", "..", ".well-known/acme-challenge/token"] {
            assert_code(
                write(&storage, vec![message(name, b"x")]).await,
                tonic::Code::InvalidArgument,
            );
            assert!(missing(&storage, name).await);
        }
        write(&storage, vec![message("a\tb", b"x")]).await.unwrap();
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_finalized_resume_returns_the_original_resource() {
        let (_dir, storage) = setup(false).await;
        let upload = start(&storage, "replayed", None).await;
        let original = write(&storage, vec![resume(&upload, 0, b"original", true)])
            .await
            .unwrap();
        write(&storage, vec![message("replayed", b"replacement")])
            .await
            .unwrap();
        let replayed = write(&storage, vec![resume(&upload, 0, b"ignored", true)])
            .await
            .unwrap();
        assert_eq!(replayed, original);
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_bidi_half_close_reports_unacknowledged_persisted_size() {
        let (_dir, storage) = setup(false).await;
        for (name, data, flush) in [
            ("half-close", b"abc".as_slice(), false),
            ("flushed", b"abc".as_slice(), true),
            ("empty", b"".as_slice(), false),
        ] {
            let upload = start(&storage, name, None).await;
            let mut message = resume(&upload, 0, data, false);
            message.flush = flush;
            let mut stream = tonic::codegen::tokio_stream::iter([Ok(message)]);
            let (tx, mut rx) = mpsc::channel(2);
            let result = storage
                .receive_write(&mut stream, None, Some(&tx), EndOfStream::NotChecked)
                .await
                .unwrap();
            assert_eq!(
                result,
                pb::bidi_write_object_response::WriteStatus::PersistedSize(data.len() as i64)
            );
            drop(tx);
            let response = rx.recv().await.unwrap().unwrap();
            assert_eq!(response.write_status, Some(result));
            assert!(rx.recv().await.is_none());
            assert!(missing(&storage, name).await);
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_read_failures_use_shared_storage_status() {
        let (dir, storage) = setup(false).await;
        write(&storage, vec![message("short-blob", b"abc")])
            .await
            .unwrap();
        let shard = std::fs::read_dir(dir.0.join("blobs"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let blob = std::fs::read_dir(shard)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::OpenOptions::new()
            .write(true)
            .open(blob)
            .unwrap()
            .set_len(0)
            .unwrap();
        let mut stream = storage
            .read_object(
                Request::new(pb::ReadObjectRequest {
                    bucket: "projects/_/buckets/grpc-io".into(),
                    object: "short-blob".into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let read_error = stream.next().await.unwrap().unwrap_err();
        assert!(
            read_error
                .message()
                .starts_with("Object storage is unavailable")
        );
        let read_code = read_error.code();
        let (status, _): (_, Vec<pb::BidiReadObjectResponse>) = fixtures::rpc_status(
            "/google.storage.v2.Storage/BidiReadObject",
            None,
            &[pb::BidiReadObjectRequest {
                read_object_spec: Some(pb::BidiReadObjectSpec {
                    bucket: "projects/_/buckets/grpc-io".into(),
                    object: "short-blob".into(),
                    ..Default::default()
                }),
                read_ranges: [7, 8, 9]
                    .into_iter()
                    .map(|read_id| pb::ReadRange {
                        read_id,
                        ..Default::default()
                    })
                    .collect(),
            }],
            async |request| {
                super::super::dispatch(
                    super::super::service(storage.state.clone(), 1024, storage.compat_profile),
                    request,
                )
                .await
                .unwrap()
            },
        )
        .await;
        assert_eq!([read_code, status.code()], [tonic::Code::Unavailable; 2]);
        let rpc = crate::grpc::generated::google::rpc::Status::decode(status.details()).unwrap();
        let detail = pb::BidiReadObjectError::decode(rpc.details[0].value.as_slice()).unwrap();
        let states = detail
            .read_range_errors
            .into_iter()
            .map(|range| (range.read_id, range.status.unwrap().code))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            states,
            [
                (7, tonic::Code::Unavailable as i32),
                (8, tonic::Code::Aborted as i32),
                (9, tonic::Code::Aborted as i32)
            ]
            .into()
        );
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_disconnected_resumable_stream_releases_its_lease() {
        let (dir, storage) = setup(false).await;
        let upload = start(&storage, "disconnected", None).await;
        let mut stream = tonic::codegen::tokio_stream::iter([
            Ok(resume(&upload, 0, b"abc", false)),
            Err(Status::cancelled("client disconnected")),
        ]);
        assert_code(
            storage
                .receive_write(&mut stream, None, None, EndOfStream::Required)
                .await,
            tonic::Code::Cancelled,
        );
        assert_eq!(
            query(&storage, &upload, None).await.unwrap(),
            pb::query_write_status_response::WriteStatus::PersistedSize(3)
        );
        write(&storage, vec![resume(&upload, 3, b"def", true)])
            .await
            .unwrap();
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
        storage.state.catalog.close().await.unwrap();
    }

    #[test]
    fn grpc_io_range_and_masks() {
        assert_eq!(
            Status::from(json::JsonError::new(
                hyper::StatusCode::NOT_MODIFIED,
                "notModified",
                "Object condition matched"
            ))
            .code(),
            tonic::Code::FailedPrecondition
        );
        let unsupported = Status::from(json::JsonError::new(
            hyper::StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            "Unsupported object option",
        ));
        assert_eq!(unsupported.code(), tonic::Code::Unimplemented);
        assert!(!unsupported.details().is_empty());
        let mismatch = verify_crc32c(&pb::ChecksummedData {
            content: b"hello".to_vec().into(),
            crc32c: Some(1),
        })
        .unwrap_err();
        assert_eq!(mismatch.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            mismatch.message(),
            "Checksummed data message crc32c mismatch: server computed: '9a71bb4c', client provided '00000001'."
        );
        let unknown = mask_object(
            pb::Object::default(),
            fixtures::mask(&["noSuchField"]).as_ref(),
        )
        .unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            unknown.message(),
            "There is no field for this resource named 'noSuchField'."
        );
        assert_eq!(read_range(15, -5, 3).unwrap(), (10, 3));
        assert_eq!(read_range(15, i64::MIN, 0).unwrap(), (0, 15));
        assert_eq!(read_range(15, 15, 0).unwrap(), (15, 0));
        assert_code(read_range(15, 16, 0), tonic::Code::OutOfRange);
        assert_code(read_range(15, 0, -1), tonic::Code::InvalidArgument);
        let object = mask_object(
            pb::Object {
                name: "a".into(),
                size: 12,
                ..Default::default()
            },
            fixtures::mask(&["name"]).as_ref(),
        )
        .unwrap();
        assert_eq!(object.name, "a");
        assert_eq!(object.size, 0);
        assert_code(
            mask_object(
                pb::Object::default(),
                fixtures::mask(&["owner.entity"]).as_ref(),
            ),
            tonic::Code::Unimplemented,
        );
    }

    #[tokio::test]
    async fn grpc_listing_uses_proto_errors_and_preserves_page_tokens() {
        let (_dir, storage) = setup(false).await;
        let base = pb::ListObjectsRequest {
            parent: "projects/_/buckets/grpc-io".into(),
            ..Default::default()
        };
        for (request, field) in [
            (
                pb::ListObjectsRequest {
                    soft_deleted: true,
                    versions: true,
                    ..base.clone()
                },
                "soft_deleted",
            ),
            (
                pb::ListObjectsRequest {
                    match_glob: "**/*.txt".into(),
                    delimiter: "-".into(),
                    ..base.clone()
                },
                "match_glob",
            ),
            (
                pb::ListObjectsRequest {
                    match_glob: "[".into(),
                    ..base.clone()
                },
                "opening '[' at index 0",
            ),
            (
                pb::ListObjectsRequest {
                    page_token: "tampered".into(),
                    ..base.clone()
                },
                "page_token",
            ),
        ] {
            let error = storage
                .list_objects(Request::new(request), None)
                .await
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(error.message().contains(field), "{error}");
        }
        for name in ["a&=1", "b&=2"] {
            write(&storage, vec![message(name, b"x")]).await.unwrap();
        }
        let query = crate::storage::ObjectListQuery {
            bucket: "grpc-io".into(),
            ..Default::default()
        };
        let token = json::encode_object_page_token(
            &query,
            json::Projection::NoAcl,
            &crate::storage::ObjectListCursor {
                name: "a&=1".into(),
                is_prefix: false,
                generation: None,
            },
        );
        let page = storage
            .list_objects(
                Request::new(pb::ListObjectsRequest {
                    page_size: 1,
                    page_token: token,
                    ..base
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(page.objects.len(), 1);
        assert_eq!(page.objects[0].name, "b&=2");
        storage.state.catalog.close().await.unwrap();
    }

    /// Every top-level `Object` field, in proto order: the read mask the Java SDK sends for
    /// GetObject and ListObjects.
    const ALL_OBJECT_FIELDS: [&str; 33] = [
        "name",
        "bucket",
        "etag",
        "generation",
        "restore_token",
        "metageneration",
        "storage_class",
        "size",
        "content_encoding",
        "content_disposition",
        "cache_control",
        "acl",
        "content_language",
        "delete_time",
        "finalize_time",
        "content_type",
        "create_time",
        "component_count",
        "checksums",
        "update_time",
        "kms_key",
        "update_storage_class_time",
        "temporary_hold",
        "retention_expire_time",
        "metadata",
        "contexts",
        "event_based_hold",
        "owner",
        "customer_encryption",
        "custom_time",
        "soft_delete_time",
        "hard_delete_time",
        "retention",
    ];

    #[tokio::test]
    async fn grpc_derived_writes_return_owner_and_acl() {
        let (_dir, storage) = setup(true).await;
        let admin = fixtures::caller(&storage, "admin");
        let bucket = "projects/_/buckets/grpc-io";
        let pb::bidi_write_object_response::WriteStatus::Resource(source) =
            write_as(&storage, vec![message("source", b"data")], admin)
                .await
                .unwrap()
        else {
            panic!("finished write must return metadata")
        };
        let composed = storage
            .compose_object(
                Request::new(pb::ComposeObjectRequest {
                    destination: Some(pb::Object {
                        bucket: bucket.into(),
                        name: "composed".into(),
                        ..Default::default()
                    }),
                    source_objects: vec![pb::compose_object_request::SourceObject {
                        name: "source".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner();
        let rewritten = storage
            .rewrite_object(
                Request::new(pb::RewriteObjectRequest {
                    source_bucket: bucket.into(),
                    source_object: "source".into(),
                    destination_bucket: bucket.into(),
                    destination_name: "rewritten".into(),
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner()
            .resource
            .unwrap();
        let moved = storage
            .move_object(
                Request::new(pb::MoveObjectRequest {
                    bucket: bucket.into(),
                    source_object: "composed".into(),
                    destination_object: "moved".into(),
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner();
        storage
            .state
            .catalog
            .patch_bucket(
                "grpc-io",
                crate::bucket::BucketMetadataUpdate {
                    versioning_enabled: Some(false),
                    soft_delete_retention: Some(604800),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        storage
            .state
            .catalog
            .delete_object(
                ObjectTarget::new("grpc-io", "rewritten", Some(rewritten.generation)),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let restored = storage
            .restore_object(
                Request::new(pb::RestoreObjectRequest {
                    bucket: bucket.into(),
                    object: "rewritten".into(),
                    generation: rewritten.generation,
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner();
        for object in [composed, rewritten, moved, restored] {
            assert_eq!(object.owner, source.owner, "{} owner", object.name);
            assert!(!object.acl.is_empty(), "{} ACL", object.name);
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_derived_write_views_respect_acl_permissions_and_uniform_access() {
        let (_dir, storage) = setup(true).await;
        let admin = fixtures::caller(&storage, "admin");
        let user = fixtures::caller(&storage, "user");
        let bucket = "projects/_/buckets/grpc-io";
        write_as(&storage, vec![message("source", b"data")], admin)
            .await
            .unwrap();
        let moved = storage
            .move_object(
                Request::new(pb::MoveObjectRequest {
                    bucket: bucket.into(),
                    source_object: "source".into(),
                    destination_object: "moved".into(),
                    ..Default::default()
                }),
                user,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(moved.owner.is_none());
        assert!(moved.acl.is_empty());
        storage
            .state
            .catalog
            .patch_bucket(
                "grpc-io",
                crate::bucket::BucketMetadataUpdate {
                    uniform_bucket_level_access_enabled: Some(true),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let composed = storage
            .compose_object(
                Request::new(pb::ComposeObjectRequest {
                    destination: Some(pb::Object {
                        bucket: bucket.into(),
                        name: "uniform".into(),
                        ..Default::default()
                    }),
                    source_objects: vec![pb::compose_object_request::SourceObject {
                        name: "moved".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(composed.owner.is_none());
        assert!(composed.acl.is_empty());
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_io_write_returns_acl_and_owner_and_read_ends_with_data() {
        let (_dir, storage) = setup(true).await;
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("authorization", "Bearer admin".parse().unwrap());
        let admin = storage.authenticate(&request).unwrap();
        let written = storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter([Ok(message("written", b"hello"))]),
                admin,
                None,
                EndOfStream::Required,
            )
            .await
            .unwrap();
        let pb::bidi_write_object_response::WriteStatus::Resource(resource) = written else {
            panic!("a finished write answers with the resource");
        };
        assert!(!resource.acl.is_empty());
        assert!(resource.owner.is_some());
        assert!(
            resource
                .acl
                .iter()
                .any(|entry| entry.entity_alt.starts_with("project-owners-")),
            "a project-team entry repeats its entity under the project ID"
        );

        let mut read = storage
            .read_object(
                Request::new(pb::ReadObjectRequest {
                    bucket: "projects/_/buckets/grpc-io".into(),
                    object: "written".into(),
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner();
        let first = read.next().await.unwrap().unwrap();
        assert_eq!(first.checksummed_data.unwrap().content.as_ref(), b"hello");
        assert!(first.metadata.is_some());
        assert!(
            read.next().await.is_none(),
            "no empty message follows the last data message"
        );
    }

    #[tokio::test]
    async fn grpc_io_full_read_masks_return_acl_and_owner() {
        let (_dir, storage) = setup(true).await;
        let mut request = Request::new(());
        request
            .metadata_mut()
            .insert("authorization", "Bearer admin".parse().unwrap());
        let admin = storage.authenticate(&request).unwrap();
        storage
            .receive_write(
                &mut tonic::codegen::tokio_stream::iter([Ok(message("masked", b"masked"))]),
                admin,
                None,
                EndOfStream::Required,
            )
            .await
            .unwrap();
        let get = |mask: Option<prost_types::FieldMask>, caller| {
            storage.get_object(
                Request::new(pb::GetObjectRequest {
                    bucket: "projects/_/buckets/grpc-io".into(),
                    object: "masked".into(),
                    read_mask: mask,
                    ..Default::default()
                }),
                caller,
            )
        };
        let full = get(fixtures::mask(&ALL_OBJECT_FIELDS), admin)
            .await
            .unwrap()
            .into_inner();
        assert_eq!((full.name.as_str(), full.size), ("masked", 6));
        assert!(!full.etag.is_empty());
        assert!(full.update_storage_class_time.is_some());
        // Google reports finalize_time on every ordinary object.
        assert!(full.finalize_time.is_some());
        // create_time is the finalize time cut to milliseconds.
        let (finalized, created) = (full.finalize_time.unwrap(), full.create_time.unwrap());
        assert_eq!(finalized.seconds, created.seconds);
        assert_eq!(finalized.nanos / 1_000_000 * 1_000_000, created.nanos);
        assert!(!full.acl.is_empty());
        for entry in &full.acl {
            assert!(!entry.entity.is_empty() && !entry.role.is_empty());
            assert!(!entry.id.is_empty() && !entry.etag.is_empty());
        }
        assert!(full.acl.iter().any(|entry| {
            entry
                .project_team
                .as_ref()
                .is_some_and(|team| team.team == "owners")
        }));
        assert!(
            full.owner
                .as_ref()
                .is_some_and(|owner| !owner.entity.is_empty())
        );
        // No local object has a KMS key, contexts or a restore token, so none is invented.
        assert!(full.kms_key.is_empty() && full.contexts.is_none());
        assert!(full.restore_token.is_none());

        let star = get(fixtures::mask(&["*"]), admin)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(star, full);

        let acl_only = get(fixtures::mask(&["name", "acl"]), admin)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(acl_only.acl, full.acl);
        assert!(acl_only.owner.is_none() && acl_only.etag.is_empty());
        assert_eq!(acl_only.size, 0);

        // The default mask omits ACL and owner.
        let default = get(None, admin).await.unwrap().into_inner();
        assert!(default.acl.is_empty() && default.owner.is_none());
        assert_eq!(default.etag, full.etag);

        // A viewer may read the object but not its ACL; the request still succeeds, as JSON
        // projection=full does.
        request
            .metadata_mut()
            .insert("authorization", "Bearer reader".parse().unwrap());
        let reader = storage.authenticate(&request).unwrap();
        let viewed = get(fixtures::mask(&ALL_OBJECT_FIELDS), reader)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(viewed.size, 6);
        assert!(viewed.acl.is_empty() && viewed.owner.is_none());

        let listed = storage
            .list_objects(
                Request::new(pb::ListObjectsRequest {
                    parent: "projects/_/buckets/grpc-io".into(),
                    read_mask: fixtures::mask(&ALL_OBJECT_FIELDS),
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.objects, std::slice::from_ref(&full));

        let mut read = storage
            .read_object(
                Request::new(pb::ReadObjectRequest {
                    bucket: "projects/_/buckets/grpc-io".into(),
                    object: "masked".into(),
                    read_mask: fixtures::mask(&["metadata.name", "metadata.acl"]),
                    ..Default::default()
                }),
                admin,
            )
            .await
            .unwrap()
            .into_inner();
        let metadata = read.next().await.unwrap().unwrap().metadata.unwrap();
        assert_eq!(metadata.name, "masked");
        assert_eq!(metadata.acl, full.acl);
        assert!(metadata.owner.is_none());
        assert_code(
            get(fixtures::mask(&["owner.entity"]), admin).await,
            tonic::Code::Unimplemented,
        );
        // A checksum sub-path keeps only that digest, as the Java SDK lists objects.
        let sums = full.checksums.clone().unwrap();
        assert!(sums.crc32c.is_some() && !sums.md5_hash.is_empty());
        let md5_only = get(fixtures::mask(&["name", "checksums.md5_hash"]), admin)
            .await
            .unwrap()
            .into_inner();
        let md5_only = md5_only.checksums.unwrap();
        assert_eq!(
            (md5_only.crc32c, md5_only.md5_hash),
            (None, sums.md5_hash.clone())
        );
        let crc_only = get(fixtures::mask(&["checksums.crc32c"]), admin)
            .await
            .unwrap()
            .into_inner()
            .checksums
            .unwrap();
        assert_eq!(
            (crc_only.crc32c, crc_only.md5_hash.is_empty()),
            (sums.crc32c, true)
        );
        storage.state.catalog.close().await.unwrap();
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
            .get_object(live("grpc-io", "cold"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(stored.storage_class.as_str(), "COLDLINE");
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
    async fn grpc_class_and_stale_conditions_keep_error_precedence() {
        let (_dir, storage) = setup(false).await;
        write(&storage, vec![message("source", b"source")])
            .await
            .unwrap();
        write(&storage, vec![message("held", b"preserved")])
            .await
            .unwrap();
        let before = storage
            .state
            .catalog
            .get_object(live("grpc-io", "held"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        let mut snapshots = Vec::new();
        for class in ["REGIONAL", "standard", "UNKNOWN"] {
            let mut request = message("held", b"x");
            let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
                &mut request.first_message
            else {
                unreachable!()
            };
            spec.resource.as_mut().unwrap().storage_class = class.into();
            spec.if_generation_match = Some(0);
            let resumable_spec = spec.clone();
            let status = write(&storage, vec![request]).await.unwrap_err();
            snapshots.push(json!([
                "write",
                class,
                format!("{:?}", status.code()),
                status.message()
            ]));
            let status = storage
                .start_resumable_write(
                    Request::new(pb::StartResumableWriteRequest {
                        write_object_spec: Some(resumable_spec),
                        ..Default::default()
                    }),
                    None,
                )
                .await
                .unwrap_err();
            snapshots.push(json!([
                "start-resumable",
                class,
                format!("{:?}", status.code()),
                status.message()
            ]));
            for (label, source) in [("rewrite-destination", None), ("rewrite-source", Some(0))] {
                let status = storage
                    .rewrite_object(
                        Request::new(pb::RewriteObjectRequest {
                            source_bucket: "projects/_/buckets/grpc-io".into(),
                            source_object: "source".into(),
                            destination_bucket: "projects/_/buckets/grpc-io".into(),
                            destination_name: "held".into(),
                            destination: Some(pb::Object {
                                storage_class: class.into(),
                                ..Default::default()
                            }),
                            if_generation_match: Some(0),
                            if_source_generation_match: source,
                            ..Default::default()
                        }),
                        None,
                    )
                    .await
                    .unwrap_err();
                snapshots.push(json!([
                    label,
                    class,
                    format!("{:?}", status.code()),
                    status.message()
                ]));
            }
            let status = storage
                .update_bucket(
                    Request::new(pb::UpdateBucketRequest {
                        bucket: Some(pb::Bucket {
                            name: "projects/_/buckets/grpc-io".into(),
                            storage_class: class.into(),
                            ..Default::default()
                        }),
                        update_mask: fixtures::mask(&["storage_class"]),
                        if_metageneration_match: Some(999),
                        ..Default::default()
                    }),
                    None,
                )
                .await
                .unwrap_err();
            snapshots.push(json!([
                "bucket-update",
                class,
                format!("{:?}", status.code()),
                status.message()
            ]));
            assert_eq!(
                storage
                    .state
                    .catalog
                    .get_object(live("grpc-io", "held"), Default::default(), PERMISSIVE)
                    .await
                    .unwrap(),
                before,
                "{class}"
            );
        }
        let mut expected = Vec::new();
        for class in ["REGIONAL", "standard", "UNKNOWN"] {
            for label in [
                "write",
                "start-resumable",
                "rewrite-destination",
                "rewrite-source",
                "bucket-update",
            ] {
                let (code, message) = if label == "rewrite-source" {
                    (
                        "FailedPrecondition",
                        "At least one of the pre-conditions you specified did not hold.".to_owned(),
                    )
                } else if class == "REGIONAL" {
                    ("Unimplemented", if matches!(label, "write" | "start-resumable") {
                        "gcs-mimic does not support gRPC unsupported metadata change: storage_class=REGIONAL: storage_class requires a region or zone; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
                    } else {
                        "gcs-mimic does not support bucket API setting storage_class requires a region or zone; omit this setting (for example, create a bucket with only name)."
                    }.to_owned())
                } else {
                    ("InvalidArgument", match label {
                        "write" | "start-resumable" => "Invalid object storage_class; use STANDARD, NEARLINE, COLDLINE or ARCHIVE".into(),
                        "bucket-update" => format!("Invalid storage class \"{class}\""),
                        _ => "The storage class you specified is not valid.".into(),
                    })
                };
                expected.push(json!([label, class, code, message]));
            }
        }
        assert_eq!(snapshots, expected);
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
        assert_code(write(&storage, vec![first]).await, tonic::Code::OutOfRange);
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
            assert_code(
                write(&storage, vec![bad]).await,
                tonic::Code::InvalidArgument,
            );
            assert_eq!(
                storage
                    .state
                    .catalog
                    .get_object(live("grpc-io", "object"), Default::default(), PERMISSIVE)
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
        assert_code(
            write(&storage, vec![losing]).await,
            tonic::Code::FailedPrecondition,
        );
        write(&storage, vec![message("object", b"new")])
            .await
            .unwrap();
        let get = pb::GetObjectRequest {
            bucket: "projects/_/buckets/grpc-io".into(),
            object: "object".into(),
            generation: first.generation,
            read_mask: fixtures::mask(&["generation", "size"]),
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
        assert_eq!(data.content, b"gin".as_slice());
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
        assert_code(
            storage.get_object(Request::new(get), None).await,
            tonic::Code::NotFound,
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
                EndOfStream::Required,
            )
            .await
            .unwrap();
        request
            .metadata_mut()
            .insert("authorization", "Bearer reader".parse().unwrap());
        let reader = storage.authenticate(&request).unwrap();
        assert_code(
            storage
                .receive_write(
                    &mut tonic::codegen::tokio_stream::iter([Ok(message("private", b"denied"))]),
                    reader,
                    None,
                    EndOfStream::Required,
                )
                .await,
            tonic::Code::PermissionDenied,
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
        assert_code(
            storage.write_spec(unsupported_spec, None, admin).await,
            tonic::Code::Unimplemented,
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
            .get_object(live("grpc-io", name), Default::default(), PERMISSIVE)
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
            assert_code(
                write(&storage, vec![resume(&fresh, 0, b"abc", finish)]).await,
                tonic::Code::NotFound,
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
        let staged = blob.staging_path(&dir.0);
        assert!(staged.exists());
        assert_eq!(cancel(&storage, &partial, None).await, Ok(()));
        assert!(!staged.exists());
        assert!(!blob.blob_path(&dir.0).exists());
        assert_code(
            write(&storage, vec![resume(&partial, 3, b"def", true)]).await,
            tonic::Code::NotFound,
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
                EndOfStream::Required,
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
            assert_code(
                write(&storage, vec![rejected]).await,
                tonic::Code::Unimplemented,
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
            assert_code(
                write(&storage, vec![bad.clone()]).await,
                tonic::Code::InvalidArgument,
            );
            assert_eq!(
                storage
                    .state
                    .catalog
                    .get_object(live("grpc-io", name), Default::default(), PERMISSIVE)
                    .await
                    .unwrap_err(),
                ObjectError::ObjectNotFound
            );
            assert_code(
                storage
                    .query_write_status(
                        Request::new(pb::QueryWriteStatusRequest {
                            upload_id: upload,
                            ..Default::default()
                        }),
                        None,
                    )
                    .await,
                tonic::Code::NotFound,
            );
            assert_code(write(&storage, vec![bad]).await, tonic::Code::NotFound);
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
        assert_code(
            write(&storage, vec![bad]).await,
            tonic::Code::InvalidArgument,
        );
        assert_eq!(
            storage
                .state
                .catalog
                .get_object(live("grpc-io", "final-crc"), Default::default(), PERMISSIVE)
                .await
                .unwrap_err(),
            ObjectError::ObjectNotFound
        );
        storage.state.catalog.close().await.unwrap();
    }
}
