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
                position.require_resume_start()?;
                let resource = self
                    .completed_resource(lease.session.completion.as_ref(), caller, bucket)
                    .await?;
                drop(lease);
                let result = WriteStatus::Resource(resource);
                send_status(responses, &result).await?;
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
            let reported = if message.finish_write || message.flush || message.state_lookup {
                send_status(responses, &result).await?
            } else {
                Reported::No
            };
            if message.finish_write {
                return Ok(result);
            }
            position = MessagePosition::Later;
            let Some(next) = stream.next().await.transpose()? else {
                if reported == Reported::No {
                    send_status(responses, &result).await?;
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
        send_status(responses, &result).await?;
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

impl MessagePosition {
    fn require_resume_start(self) -> Result<(), Status> {
        match self {
            Self::First => Ok(()),
            Self::Later => Err(Status::failed_precondition(
                "Upload finalized by another stream; start a new upload",
            )),
        }
    }
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
    responses: Option<&Responses>,
    status: &pb::bidi_write_object_response::WriteStatus,
) -> Result<Reported, Status> {
    let Some(tx) = responses else {
        return Ok(Reported::No);
    };
    tx.send(Ok(pb::BidiWriteObjectResponse {
        write_status: Some(status.clone()),
        ..Default::default()
    }))
    .await
    .map_err(|_| Status::cancelled("Response stream closed"))?;
    Ok(Reported::Yes)
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
mod tests;
