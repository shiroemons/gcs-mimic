use super::{
    ResponseStream, Storage,
    mask::ReadMask,
    object_io::naming_object,
    pb,
    routing::{Admitted, Route},
    status::unsupported,
};
use crate::{
    auth::{self, Caller},
    bucket::BucketAccess,
    csek::CustomerKey,
    grpc::{
        generated::google::rpc::Status as RpcStatus,
        validate::{self, NegativeGeneration},
    },
    object::{ObjectConditions, ObjectError, ObjectRecord, generation::Generation},
    storage::{Finality, ObjectReader},
};
use prost::Message;
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, SystemTime},
};
use tokio::sync::mpsc;
use tonic::{Request, Response, Status, Streaming};

const MAX_RANGES: usize = 100;
const MAX_HANDLES: usize = 1024;
const HANDLE_TTL: Duration = Duration::from_secs(600);
const READ_ERROR_TYPE: &str = "type.googleapis.com/google.storage.v2.BidiReadObjectError";

struct Selected {
    record: ObjectRecord,
    reader: ObjectReader,
    size: u64,
    /// An appendable generation snapshot: reads stop at the size persisted when it was opened.
    finality: Finality,
}

#[derive(Clone, Copy)]
struct HandleToken([u8; 32]);

impl HandleToken {
    fn matches(self, other: &[u8]) -> bool {
        other.len() == self.0.len()
            && self
                .0
                .iter()
                .zip(other)
                .fold(0, |difference, (a, b)| difference | (a ^ b))
                == 0
    }
}

#[derive(Clone)]
struct Handle {
    token: HandleToken,
    bucket: String,
    name: String,
    generation: i64,
    size: u64,
    finality: Finality,
    project: Option<String>,
    expires: SystemTime,
}

/// Read snapshots belong to one server state and survive individual gRPC connections.
#[derive(Default)]
pub(crate) struct ReadHandles(Mutex<VecDeque<Handle>>);

impl ReadHandles {
    fn insert(&self, handle: Handle, now: SystemTime) {
        let mut entries = crate::sync::lock(&self.0);
        entries.retain(|entry| entry.expires > now);
        if entries.len() == MAX_HANDLES {
            entries.pop_front();
        }
        entries.push_back(handle);
    }

    fn find(&self, token: &[u8], now: SystemTime) -> Option<Handle> {
        let mut entries = crate::sync::lock(&self.0);
        entries.retain(|entry| entry.expires > now);
        entries
            .iter()
            .find(|entry| entry.token.matches(token))
            .cloned()
    }

    fn remove(&self, token: HandleToken) {
        crate::sync::lock(&self.0).retain(|entry| !entry.token.matches(&token.0));
    }
}

fn project(access: &BucketAccess) -> Option<&str> {
    match access {
        BucketAccess::Policy { project, .. } => project.as_deref(),
        BucketAccess::Permissive | BucketAccess::Admin(_) => None,
    }
}

fn check_handle_project(expected: Option<&str>, access: &BucketAccess) -> Result<(), Status> {
    if expected != project(access) {
        return Err(Status::failed_precondition(
            "Read handle belongs to a different bucket owner; reopen without read_handle",
        ));
    }
    Ok(())
}

/// The object fields in BidiReadObject's first response. The deprecated `read_mask` defaults to
/// every field except `metadata.owner` and `metadata.acl`, and `*` means every field
/// (storage.proto lines 1183-1187); the `x-goog-fieldmask` header, which the proto names as the
/// field's alternative, takes `*` the same way. Any other mask is refused: the field's text,
/// copied from ReadObjectRequest, names a response field BidiReadObjectResponse does not have,
/// so which response paths a mask selects is undefined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MetadataFields {
    /// Every field except the ACL and the owner.
    Default,
    /// Every field, the ACL and the owner included.
    All,
}

/// A read mask other than the default and `*`.
struct UnsupportedReadMask;

impl TryFrom<ReadMask<'_>> for MetadataFields {
    type Error = UnsupportedReadMask;

    fn try_from(mask: ReadMask<'_>) -> Result<Self, Self::Error> {
        match mask {
            ReadMask::Default => Ok(Self::Default),
            ReadMask::Wildcard => Ok(Self::All),
            ReadMask::MixedWildcard(_) | ReadMask::Paths(_) => Err(UnsupportedReadMask),
        }
    }
}

impl From<MetadataFields> for ReadMask<'static> {
    fn from(fields: MetadataFields) -> Self {
        match fields {
            MetadataFields::Default => Self::Default,
            MetadataFields::All => Self::Wildcard,
        }
    }
}

/// What a valid `read_object_spec` selects.
#[derive(Debug, PartialEq, Eq)]
struct ReadSpec {
    /// The generation to read, or `None` for the live object.
    generation: Option<Generation>,
    /// The object fields the spec's `read_mask` selects.
    fields: MetadataFields,
}

#[expect(
    deprecated,
    reason = "the deprecated read_mask field is still documented, so it is read and checked"
)]
fn validate_spec(spec: &pb::BidiReadObjectSpec) -> Result<ReadSpec, Status> {
    const MESSAGE: &str =
        "Generation conditions must be nonnegative; use generation 0 for the live object";
    const REFUSED: &str = "BidiReadObject routing_token or read_mask";
    let generation = validate::selected_generation(spec.generation)
        .map_err(|NegativeGeneration| Status::invalid_argument(MESSAGE))?;
    // Negative condition values are compared like any other value, as GetObject and ReadObject
    // do (recorded 2026-11-18, case grpc-probe-inferred-conditions). Not recorded for
    // BidiReadObject itself; inferred from those two reads of the same object.
    let Ok(fields) = MetadataFields::try_from(ReadMask::from(spec.read_mask.as_ref())) else {
        return Err(unsupported(REFUSED));
    };
    if spec
        .routing_token
        .as_ref()
        .is_some_and(|token| !token.is_empty())
    {
        return Err(unsupported(REFUSED));
    }
    if spec.read_handle.is_some()
        && (spec.if_generation_match.is_some()
            || spec.if_generation_not_match.is_some()
            || spec.if_metageneration_match.is_some()
            || spec.if_metageneration_not_match.is_some())
    {
        return Err(unsupported(
            "BidiReadObject preconditions with read_handle; reopen without read_handle to apply conditions",
        ));
    }
    Ok(ReadSpec { generation, fields })
}

/// The object fields the `x-goog-fieldmask` header selects, which only `*` may name.
fn header_fields(metadata: &tonic::metadata::MetadataMap) -> Result<MetadataFields, Status> {
    match metadata.get("x-goog-fieldmask") {
        None => Ok(MetadataFields::Default),
        Some(mask) if mask == "*" => Ok(MetadataFields::All),
        Some(_) => Err(unsupported("BidiReadObject x-goog-fieldmask")),
    }
}

impl Storage {
    async fn resume_read_handle(
        &self,
        spec: &pb::BidiReadObjectSpec,
        generation: Option<Generation>,
        access: &BucketAccess,
        caller: Option<Caller<'_>>,
        key: Option<CustomerKey>,
    ) -> Result<(Selected, HandleToken), Status> {
        let handle = spec.read_handle.as_ref().ok_or_else(|| {
            Status::invalid_argument("Missing read_handle; reopen without read_handle")
        })?;
        let object_key = self.object_key(&spec.bucket, &spec.object)?;
        let (bucket, object) = object_key.parts();
        let entry = self
            .state
            .grpc_read_handles
            .find(&handle.handle, self.state.clock.now())
            .ok_or_else(|| {
                Status::failed_precondition(
                    "Read handle is unknown or expired; reopen without read_handle",
                )
            })?;
        check_handle_project(entry.project.as_deref(), access)?;
        if entry.bucket != bucket
            || entry.name != object
            || generation.is_some_and(|generation| generation.get() != entry.generation)
        {
            return Err(Status::invalid_argument(
                "Read handle does not match bucket, object or generation; use the original object or reopen without read_handle",
            ));
        }
        let target = object_key.into_target(Some(entry.generation));
        let (record, mut reader, _) = self
            .open_readable(target, Default::default(), access, key)
            .await
            .map_err(|error| {
                // A wrong or missing key is the caller's mistake, not a sign the snapshot is
                // gone, so only a vanished object invalidates the handle.
                if matches!(
                    error,
                    ObjectError::ObjectNotFound | ObjectError::BucketNotFound
                ) {
                    self.state.grpc_read_handles.remove(entry.token);
                }
                super::access_status(naming_object(bucket, object)(error), caller)
            })?;
        let size = record.size.min(entry.size);
        reader
            .set_range(0, size)
            .map_err(super::status::read_failure)?;
        Ok((
            Selected {
                record,
                reader,
                size,
                finality: entry.finality,
            },
            entry.token,
        ))
    }

    pub(super) async fn bidi_read_object(
        &self,
        request: Request<Streaming<pb::BidiReadObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::BidiReadObjectResponse>>, Status> {
        let header_fields = header_fields(request.metadata())?;
        let (_, extensions, mut input) = request.into_parts();
        let first = input.message().await?.ok_or_else(|| {
            Status::invalid_argument(
                "First message must contain read_object_spec; specify bucket and object",
            )
        })?;
        let spec = first.read_object_spec.ok_or_else(|| {
            Status::invalid_argument(
                "First message requires read_object_spec; specify bucket and object",
            )
        })?;
        let ReadSpec { generation, fields } = validate_spec(&spec)?;
        // `*` in either the field or the header selects every field.
        let fields = fields.max(header_fields);
        // Real Cloud Storage refused BidiReadObject for the test project, so nothing was
        // recorded. These rules are local decisions that mirror ReadObject, and a resumed
        // read must repeat its key because a handle never stores one.
        let key = super::customer_key::key(spec.common_object_request_params.as_ref())?;
        Admitted::of(&extensions)?.compare(&[Route::Bucket(Some(spec.bucket.as_str()))])?;
        let object_key = self.object_key(&spec.bucket, &spec.object)?;
        let (bucket, object) = object_key.parts();
        let access = self.access(caller, bucket, auth::OBJECTS_GET).await?;
        let resumed = spec.read_handle.is_some();
        let (mut selected, token) = if resumed {
            self.resume_read_handle(&spec, generation, &access, caller, key)
                .await?
        } else {
            let conditions = ObjectConditions::from(&spec);
            let target = object_key.into_target(generation.map(Generation::get));
            let (record, reader, finality) = self
                .open_readable(target, conditions, &access, key)
                .await
                .map_err(|e| super::access_status(naming_object(bucket, object)(e), caller))?;
            let selected = Selected {
                size: record.size,
                record,
                reader,
                finality,
            };
            let mut token = HandleToken([0; 32]);
            getrandom::fill(&mut token.0)
                .map_err(|_| Status::internal("Cannot create read handle; retry the read"))?;
            let now = self.state.clock.now();
            let expires = now.checked_add(HANDLE_TTL).ok_or_else(|| {
                Status::internal("Cannot issue read handle: clock exceeds supported range")
            })?;
            self.state.grpc_read_handles.insert(
                Handle {
                    token,
                    bucket: bucket.into(),
                    name: object.into(),
                    generation: selected.record.generation.get(),
                    size: selected.size,
                    finality,
                    expires,
                    project: project(&access).map(str::to_owned),
                },
                now,
            );
            (selected, token)
        };
        let mut ranges = VecDeque::new();
        add_ranges(&mut ranges, &first.read_ranges, selected.size)
            .map_err(|failure| failure.into_status(queued_ids(&ranges)))?;
        let initial = pb::BidiReadObjectResponse {
            metadata: if resumed {
                None
            } else {
                Some(
                    self.first_read_metadata(
                        caller,
                        &selected.record,
                        selected.finality,
                        fields.into(),
                    )
                    .await?,
                )
            },
            read_handle: Some(pb::BidiReadHandle {
                handle: token.0.to_vec(),
            }),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel(2);
        crate::task::spawn_supervised("grpc-bidi-read", async move {
            let work = async {
                tx.send(Ok(initial))
                    .await
                    .map_err(|_| Status::cancelled("Read stream closed"))?;
                let result = serve(&mut input, &mut selected, &mut ranges, &tx).await;
                result.map_err(|failure| failure.into_status(queued_ids(&ranges)))
            };
            tokio::select! {
                _ = tx.closed() => {},
                result = work => { if let Err(error) = result { let _ = tx.send(Err(error)).await; } }
            }
        });
        Ok(Response::new(Box::pin(
            tonic::codegen::tokio_stream::wrappers::ReceiverStream::new(rx),
        )))
    }
}

const RANGE_ERROR: &str = "Range exceeds object size or has negative read_length; use offset 0 and read_length 0 to read the object";

/// A range's own failure, reported in `BidiReadObjectError.read_range_errors`.
#[derive(Clone, Debug)]
struct RangeFailure {
    read_id: i64,
    code: tonic::Code,
    message: String,
}

impl RangeFailure {
    fn to_proto(&self) -> pb::ReadRangeError {
        pb::ReadRangeError {
            read_id: self.read_id,
            status: Some(RpcStatus {
                code: self.code as i32,
                message: self.message.clone(),
                details: vec![],
            }),
        }
    }
}

/// A BidiReadObject failure before it is encoded: the stream's status, the range that caused
/// it, and the IDs a rejected message submitted after it, which never reached the queue.
struct ReadFailure {
    status: Status,
    failed: Option<RangeFailure>,
    unqueued: Vec<i64>,
}

impl ReadFailure {
    /// A failure of the stream itself, which leaves every queued range unfinished.
    fn stream(status: Status) -> Self {
        Self {
            status,
            failed: None,
            unqueued: Vec::new(),
        }
    }

    /// A submitted range the queue refused; the stream fails with the range's code and message.
    fn rejected(failed: RangeFailure, unqueued: Vec<i64>) -> Self {
        Self {
            status: Status::new(failed.code, failed.message.clone()),
            failed: Some(failed),
            unqueued,
        }
    }

    /// A queued range whose bytes could not be read; the stream fails with the read's status.
    fn read(read_id: i64, status: Status) -> Self {
        let failed = RangeFailure {
            read_id,
            code: status.code(),
            message: status.message().to_owned(),
        };
        Self {
            status,
            failed: Some(failed),
            unqueued: Vec::new(),
        }
    }

    /// The stream's final status. Existing details survive (for example an `ErrorInfo`), the
    /// outer code and message are repeated in `google.rpc.Status`, and every unfinished ID the
    /// failed one does not cover is reported ABORTED, in the order failed, queued, unqueued.
    fn into_status(self, queued: impl Iterator<Item = i64>) -> Status {
        let Self {
            status,
            failed,
            unqueued,
        } = self;
        let mut rpc = RpcStatus::decode(status.details()).unwrap_or_default();
        rpc.code = status.code() as i32;
        rpc.message = status.message().into();
        let index = rpc
            .details
            .iter()
            .position(|detail| detail.type_url == READ_ERROR_TYPE);
        let mut detail = index
            .and_then(|index| rpc.details.get(index))
            .and_then(|detail| pb::BidiReadObjectError::decode(detail.value.as_slice()).ok())
            .unwrap_or_default();
        let mut ids = detail
            .read_range_errors
            .iter()
            .map(|range| range.read_id)
            .collect::<std::collections::BTreeSet<_>>();
        if let Some(failed) = failed
            && ids.insert(failed.read_id)
        {
            detail.read_range_errors.push(failed.to_proto());
        }
        for read_id in queued.chain(unqueued) {
            if ids.insert(read_id) {
                let aborted = RangeFailure {
                    read_id,
                    code: tonic::Code::Aborted,
                    message: "Read stream failed before this range completed; retry the range"
                        .into(),
                };
                detail.read_range_errors.push(aborted.to_proto());
            }
        }
        if detail.read_range_errors.is_empty() {
            return status;
        }
        let detail = prost_types::Any {
            type_url: READ_ERROR_TYPE.into(),
            value: detail.encode_to_vec(),
        };
        if let Some(existing) = index.and_then(|index| rpc.details.get_mut(index)) {
            *existing = detail;
        } else {
            rpc.details.push(detail);
        }
        Status::with_details_and_metadata(
            status.code(),
            status.message(),
            rpc.encode_to_vec().into(),
            status.metadata().clone(),
        )
    }
}

fn queued_ids(queue: &VecDeque<pb::ReadRange>) -> impl Iterator<Item = i64> + '_ {
    queue.iter().map(|range| range.read_id)
}

/// Queues a message's ranges after resolving each against the object's size. A rejected range
/// fails the stream; the ranges it preceded in the message stay queued.
fn add_ranges(
    queue: &mut VecDeque<pb::ReadRange>,
    ranges: &[pb::ReadRange],
    size: u64,
) -> Result<(), ReadFailure> {
    if ranges.len() > MAX_RANGES {
        return Err(ReadFailure::stream(Status::invalid_argument(
            "At most 100 ranges are allowed per message; split read_ranges across multiple messages",
        )));
    }
    for (index, &range) in ranges.iter().enumerate() {
        let reject = |code, message: &str| {
            let failed = RangeFailure {
                read_id: range.read_id,
                code,
                message: message.into(),
            };
            let unqueued = ranges
                .iter()
                .skip(index + 1)
                .map(|range| range.read_id)
                .collect();
            ReadFailure::rejected(failed, unqueued)
        };
        if queue.iter().any(|pending| pending.read_id == range.read_id) {
            return Err(reject(
                tonic::Code::InvalidArgument,
                "Duplicate outstanding read_id; assign a different read_id to each range",
            ));
        }
        let out_of_range = || reject(tonic::Code::OutOfRange, RANGE_ERROR);
        // Both reasons get the same range error.
        let resolved =
            crate::grpc::validate::resolve_range(size, range.read_offset, range.read_length)
                .map_err(|_| out_of_range())?;
        queue.push_back(pb::ReadRange {
            read_offset: i64::try_from(resolved.start).map_err(|_| out_of_range())?,
            read_length: i64::try_from(resolved.len).map_err(|_| out_of_range())?,
            ..range
        });
    }
    Ok(())
}

async fn serve(
    input: &mut Streaming<pb::BidiReadObjectRequest>,
    selected: &mut Selected,
    ranges: &mut VecDeque<pb::ReadRange>,
    tx: &mpsc::Sender<Result<pb::BidiReadObjectResponse, Status>>,
) -> Result<(), ReadFailure> {
    let mut eof = false;
    loop {
        if eof && ranges.is_empty() {
            return Ok(());
        }
        // Input that has already arrived is taken first, until MAX_RANGES descriptors wait;
        // dropping an unchosen `Streaming::message` future loses nothing, as it is cancel safe.
        tokio::select! {
            biased;
            message = input.message(), if !eof && ranges.len() < MAX_RANGES => {
                if let Some(message) = message.map_err(ReadFailure::stream)? {
                    if message.read_object_spec.is_some() {
                        return Err(ReadFailure::stream(Status::invalid_argument("read_object_spec is only allowed in the first message; send only read_ranges afterward")));
                    }
                    add_ranges(ranges, &message.read_ranges, selected.size)?;
                } else { eof = true; }
            }
            // Always ready: the body reads one chunk of the next range, so that work does not
            // race the input future.
            _ = std::future::ready(()), if !ranges.is_empty() => {
                let Some(mut range) = ranges.pop_front() else { continue };
                let offset = range.read_offset;
                let data = {
                    let reader = &mut selected.reader;
                    reader.set_range(offset.cast_unsigned(), range.read_length.cast_unsigned())
                        .map_err(|error| ReadFailure::read(range.read_id, super::status::read_failure(error)))?;
                    reader.read_chunk().await.map_err(|error| ReadFailure::read(range.read_id, super::status::read_failure(error)))?
                        .unwrap_or_default()
                };
                // Readers cap chunks at 64 KiB; widening the signed pointer-sized length cannot fail.
                let length = data.len().cast_signed() as i64;
                range.read_offset += length;
                range.read_length -= length;
                let range_end = range.read_length == 0;
                let response = pb::BidiReadObjectResponse {
                    object_data_ranges: vec![pb::ObjectRangeData {
                        checksummed_data: Some(pb::ChecksummedData { crc32c: Some(crc32c::crc32c(&data)), content: data }),
                        read_range: Some(pb::ReadRange { read_offset: offset, read_length: length, read_id: range.read_id }),
                        range_end,
                    }],
                    ..Default::default()
                };
                if !range_end { ranges.push_back(range); }
                if tx.send(Ok(response)).await.is_err() { return Ok(()); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::test_support::assert_code;
    use crate::grpc::v2::access_status;
    use crate::object::{ObjectKey, ObjectTarget};
    use crate::storage::test_fixtures::PERMISSIVE;
    use std::sync::Arc;

    /// [`add_ranges`] as a client sees it: a failure is encoded against the ranges left queued.
    fn submit(
        queue: &mut VecDeque<pb::ReadRange>,
        ranges: Vec<pb::ReadRange>,
        size: u64,
    ) -> Result<(), Status> {
        add_ranges(queue, &ranges, size).map_err(|failure| failure.into_status(queued_ids(queue)))
    }

    async fn readable_state() -> Arc<crate::server::State> {
        readable_state_with_budget(1024 * 1024).await
    }

    async fn readable_state_with_budget(budget: u64) -> Arc<crate::server::State> {
        use crate::{
            auth::Auth,
            clock::SystemClock,
            storage::{Catalog, admin::LocalAdmin},
        };
        let catalog = Catalog::open_memory_for_server(
            budget,
            1024 * 1024,
            Arc::new(SystemClock),
            LocalAdmin::Enabled,
        )
        .await
        .unwrap();
        catalog
            .insert_bucket(crate::storage::test_fixtures::bucket("handle-bucket", "p"))
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(2);
        tx.send(Ok(crate::object::UploadChunk::Data(
            bytes::Bytes::from_static(b"handle data"),
        )))
        .await
        .unwrap();
        tx.send(Ok(crate::object::UploadChunk::End)).await.unwrap();
        drop(tx);
        catalog
            .put_object(
                crate::storage::test_fixtures::permissive_upload("handle-bucket", "object", 1024),
                rx,
            )
            .await
            .unwrap();
        Arc::new(crate::server::State::for_test(catalog, Auth::permissive()))
    }

    /// The generation BidiReadObject parses from `spec` before resuming its handle.
    fn issued_generation(spec: &pb::BidiReadObjectSpec) -> Option<Generation> {
        validate_spec(spec).unwrap().generation
    }

    async fn issued_spec(
        state: &Arc<crate::server::State>,
        object: &str,
    ) -> pb::BidiReadObjectSpec {
        let spec = pb::BidiReadObjectSpec {
            bucket: "projects/_/buckets/handle-bucket".into(),
            object: object.into(),
            ..Default::default()
        };
        let (status, responses): (_, Vec<pb::BidiReadObjectResponse>) =
            crate::grpc::test_support::routed_rpc_status(
                "/google.storage.v2.Storage/BidiReadObject",
                None,
                &crate::grpc::test_support::bucket_routing("handle-bucket"),
                &[pb::BidiReadObjectRequest {
                    read_object_spec: Some(spec.clone()),
                    ..Default::default()
                }],
                async |request| {
                    crate::grpc::v2::dispatch(
                        crate::grpc::v2::service(
                            state.clone(),
                            1024 * 1024,
                            crate::config::CompatProfile::Native,
                        ),
                        request,
                    )
                    .await
                    .unwrap()
                },
            )
            .await;
        assert_eq!(status.code(), tonic::Code::Ok, "{status}");
        pb::BidiReadObjectSpec {
            generation: responses[0].metadata.as_ref().unwrap().generation,
            read_handle: responses[0].read_handle.clone(),
            ..spec
        }
    }

    #[tokio::test]
    async fn read_handle_rejects_a_deleted_generation_and_discards_the_token() {
        let state = readable_state().await;
        let spec = issued_spec(&state, "object").await;
        state
            .catalog
            .delete_object(
                ObjectTarget::new("handle-bucket", "object", Some(spec.generation)),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let storage = crate::grpc::test_support::storage_with(state.clone(), 1024);
        assert_code(
            storage
                .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
                .await,
            tonic::Code::NotFound,
        );
        assert_code(
            storage
                .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
                .await,
            tonic::Code::FailedPrecondition,
        );
        state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn closed_read_handles_do_not_reserve_deleted_ram_blobs() {
        // One small blob fits with its allocation overhead; retaining it prevents a new stage.
        let state = readable_state_with_budget(300).await;
        let spec = issued_spec(&state, "object").await;
        state
            .catalog
            .delete_object(
                ObjectTarget::new("handle-bucket", "object", Some(spec.generation)),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(2);
        tx.send(Ok(crate::object::UploadChunk::Data(
            bytes::Bytes::from_static(b"handle data"),
        )))
        .await
        .unwrap();
        tx.send(Ok(crate::object::UploadChunk::End)).await.unwrap();
        drop(tx);
        state
            .catalog
            .put_object(
                crate::storage::test_fixtures::permissive_upload(
                    "handle-bucket",
                    "replacement",
                    1024 * 1024,
                ),
                rx,
            )
            .await
            .unwrap();
        state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn read_handles_expire_with_the_injected_clock() {
        let mut state = readable_state().await;
        let clock = Arc::new(crate::clock::ManualClock::new(std::time::SystemTime::now()));
        Arc::get_mut(&mut state).unwrap().clock = clock.clone();
        let spec = issued_spec(&state, "object").await;
        clock.advance(std::time::Duration::from_secs(600));
        let storage = crate::grpc::test_support::storage_with(state.clone(), 1024);
        assert_code(
            storage
                .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
                .await,
            tonic::Code::FailedPrecondition,
        );
        state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn read_handles_reopen_only_generations_still_in_the_catalog() {
        for versioned in [false, true] {
            let state = readable_state().await;
            if versioned {
                state
                    .catalog
                    .patch_bucket(
                        "handle-bucket",
                        crate::bucket::BucketMetadataUpdate {
                            versioning: Some(crate::bucket::VersioningUpdate::Set(
                                crate::bucket::Versioning::Enabled,
                            )),
                            ..Default::default()
                        },
                        Default::default(),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
            }
            let spec = issued_spec(&state, "object").await;
            let (tx, rx) = mpsc::channel(2);
            tx.send(Ok(crate::object::UploadChunk::Data(
                bytes::Bytes::from_static(b"new"),
            )))
            .await
            .unwrap();
            tx.send(Ok(crate::object::UploadChunk::End)).await.unwrap();
            drop(tx);
            state
                .catalog
                .put_object(
                    crate::storage::test_fixtures::permissive_upload(
                        "handle-bucket",
                        "object",
                        1024,
                    ),
                    rx,
                )
                .await
                .unwrap();
            let storage = crate::grpc::test_support::storage_with(state.clone(), 1024);
            let resumed = storage
                .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
                .await;
            if versioned {
                let (mut opened, _) = resumed.unwrap();
                assert_eq!(
                    opened.reader.next_chunk().await.unwrap().unwrap(),
                    b"handle data".as_slice()
                );
            } else {
                assert_code(resumed, tonic::Code::NotFound);
                let spec = issued_spec(&state, "object").await;
                state
                    .catalog
                    .delete_object(
                        ObjectTarget::new("handle-bucket", "object", None),
                        Default::default(),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
                state
                    .catalog
                    .delete_bucket("handle-bucket", Default::default(), PERMISSIVE)
                    .await
                    .unwrap();
                state
                    .catalog
                    .insert_bucket(crate::storage::test_fixtures::bucket("handle-bucket", "p"))
                    .await
                    .unwrap();
                assert_code(
                    storage
                        .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
                        .await,
                    tonic::Code::NotFound,
                );
            }
            state.catalog.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn read_handles_are_isolated_by_state_and_survive_reconnection() {
        use crate::grpc::test_support::storage_with;
        let states = [readable_state().await, readable_state().await];
        let spec = issued_spec(&states[0], "object").await;
        let reconnected = storage_with(states[0].clone(), 1024);
        let (mut resumed, token) = reconnected
            .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
            .await
            .unwrap();
        assert_eq!(
            token.0.as_slice(),
            spec.read_handle.as_ref().unwrap().handle
        );
        assert_eq!(resumed.record.generation.get(), spec.generation);
        assert_eq!(
            resumed.reader.next_chunk().await.unwrap().unwrap(),
            b"handle data".as_slice()
        );
        let other = storage_with(states[1].clone(), 1024);
        assert_code(
            other
                .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
                .await,
            tonic::Code::FailedPrecondition,
        );
        drop(resumed);
        for state in states {
            state.catalog.close().await.unwrap();
        }
    }

    #[test]
    fn read_handle_table_evicts_the_oldest_entry_at_its_bound() {
        let handles = ReadHandles::default();
        let now = SystemTime::now();
        let token = |index: usize| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
            HandleToken(bytes)
        };
        for index in 0..=MAX_HANDLES {
            handles.insert(
                Handle {
                    token: token(index),
                    bucket: "bucket".into(),
                    name: "object".into(),
                    generation: 1,
                    size: 0,
                    finality: Finality::Finalized,
                    project: None,
                    expires: now + HANDLE_TTL,
                },
                now,
            );
        }
        assert!(handles.find(&token(0).0, now).is_none());
        assert!(handles.find(&token(MAX_HANDLES).0, now).is_some());
        assert_eq!(handles.0.lock().unwrap().len(), MAX_HANDLES);
    }

    #[tokio::test]
    async fn appendable_read_handles_keep_the_issued_size_after_flush_and_finalize() {
        let state = readable_state().await;
        let mut writer = state
            .catalog
            .create_appendable(
                ObjectKey::from_parts_unchecked("handle-bucket", "log"),
                crate::storage::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
            )
            .await
            .unwrap();
        writer
            .append(0, bytes::Bytes::from_static(b"0123456789"), 1024)
            .await
            .unwrap();
        writer.flush().await.unwrap();
        let spec = issued_spec(&state, "log").await;
        writer
            .append(10, bytes::Bytes::from_static(b"abcdefghij"), 1024)
            .await
            .unwrap();
        writer.flush().await.unwrap();
        let storage = crate::grpc::test_support::storage_with(state.clone(), 1024);
        let (mut opened, _) = storage
            .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
            .await
            .unwrap();
        assert_eq!(opened.size, 10);
        assert_eq!(
            opened.reader.next_chunk().await.unwrap().unwrap(),
            b"0123456789".as_slice()
        );
        assert_code(
            submit(
                &mut VecDeque::new(),
                vec![pb::ReadRange {
                    read_offset: 15,
                    ..Default::default()
                }],
                opened.size,
            ),
            tonic::Code::OutOfRange,
        );
        drop(opened);
        writer
            .finalize(None, crate::bucket::BucketAccess::Permissive, 1024)
            .await
            .unwrap();
        let (mut opened, _) = storage
            .resume_read_handle(&spec, issued_generation(&spec), PERMISSIVE, None, None)
            .await
            .unwrap();
        assert_eq!(
            opened.reader.next_chunk().await.unwrap().unwrap(),
            b"0123456789".as_slice()
        );
        drop(opened);
        state.catalog.close().await.unwrap();
    }

    #[test]
    fn bidi_shared_error_status() {
        let unsupported = access_status(
            crate::object::ObjectError::UnsupportedMetadata("setting".into()),
            None,
        );
        assert_eq!(unsupported.code(), tonic::Code::Unimplemented);
        assert!(!unsupported.details().is_empty());
        assert_eq!(
            access_status(
                crate::object::ObjectError::Unavailable(
                    crate::cause::BackendFailure::CatalogClosed.into()
                ),
                None
            )
            .code(),
            tonic::Code::Unavailable
        );
    }

    #[test]
    fn bidi_handle_rejects_recreated_bucket_owner() {
        let access = BucketAccess::Policy {
            identity: None,
            permission: auth::Permission::ObjectsGet,
            project: Some("new-project".into()),
        };
        assert_code(
            check_handle_project(Some("original-project"), &access),
            tonic::Code::FailedPrecondition,
        );
        assert!(check_handle_project(Some("new-project"), &access).is_ok());
        assert!(check_handle_project(None, PERMISSIVE).is_ok());
    }

    #[test]
    fn bidi_range_errors_report_every_submitted_read_id() {
        let error = submit(
            &mut VecDeque::new(),
            vec![
                pb::ReadRange {
                    read_id: 1,
                    read_length: 1,
                    ..Default::default()
                },
                pb::ReadRange {
                    read_id: 2,
                    read_offset: 20,
                    ..Default::default()
                },
                pb::ReadRange {
                    read_id: 3,
                    read_length: 1,
                    ..Default::default()
                },
            ],
            10,
        )
        .unwrap_err();
        let rpc = crate::grpc::generated::google::rpc::Status::decode(error.details()).unwrap();
        assert_eq!(rpc.code, error.code() as i32);
        assert_eq!(rpc.message, error.message());
        let detail = pb::BidiReadObjectError::decode(rpc.details[0].value.as_slice()).unwrap();
        let states = detail
            .read_range_errors
            .into_iter()
            .map(|range| (range.read_id, range.status.unwrap().code))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            states,
            [
                (1, tonic::Code::Aborted as i32),
                (2, tonic::Code::OutOfRange as i32),
                (3, tonic::Code::Aborted as i32)
            ]
            .into()
        );
    }

    /// The proto requires an entry per pending read_id without naming an order. The failed range
    /// comes first, then the ranges queued by earlier messages and this one, in queue order, then
    /// the rest of the rejected message in request order.
    #[test]
    fn bidi_range_errors_list_the_failed_range_first_then_queue_and_request_order() {
        let range = |read_id, read_offset| pb::ReadRange {
            read_id,
            read_offset,
            read_length: 1,
        };
        let mut queue = VecDeque::new();
        submit(&mut queue, vec![range(9, 0), range(4, 1)], 10).unwrap();
        let error = submit(
            &mut queue,
            vec![range(1, 0), range(2, 20), range(8, 0), range(3, 0)],
            10,
        )
        .unwrap_err();
        let rpc = crate::grpc::generated::google::rpc::Status::decode(error.details()).unwrap();
        let detail = pb::BidiReadObjectError::decode(rpc.details[0].value.as_slice()).unwrap();
        let order = detail
            .read_range_errors
            .into_iter()
            .map(|range| (range.read_id, range.status.unwrap().code))
            .collect::<Vec<_>>();
        let aborted = tonic::Code::Aborted as i32;
        assert_eq!(
            order,
            [
                (2, tonic::Code::OutOfRange as i32),
                (9, aborted),
                (4, aborted),
                (1, aborted),
                (8, aborted),
                (3, aborted),
            ]
        );
    }

    #[test]
    fn bidi_duplicate_id_keeps_outer_error_in_details() {
        let range = pb::ReadRange {
            read_id: 7,
            ..Default::default()
        };
        let error = submit(&mut VecDeque::new(), vec![range, range], 10).unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        let rpc = crate::grpc::generated::google::rpc::Status::decode(error.details()).unwrap();
        assert_eq!(rpc.code, error.code() as i32);
        assert_eq!(rpc.message, error.message());
        let detail = pb::BidiReadObjectError::decode(rpc.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.read_range_errors[0].read_id, 7);
    }

    #[test]
    fn handle_tokens_reject_every_changed_byte_and_wrong_length() {
        let token = HandleToken([0xa5; 32]);
        assert!(token.matches(&token.0));
        for index in 0..32 {
            let mut changed = token.0;
            changed[index] ^= 1;
            assert!(!token.matches(&changed));
        }
        assert!(!token.matches(&token.0[..31]));
        assert!(!token.matches(&[0xa5; 33]));
    }

    #[test]
    fn stream_errors_normalize_existing_rich_status_and_preserve_details() {
        let failed = RangeFailure {
            read_id: 3,
            code: tonic::Code::OutOfRange,
            message: RANGE_ERROR.into(),
        };
        let detail = pb::BidiReadObjectError {
            read_range_errors: vec![failed.to_proto()],
        };
        let rpc = RpcStatus {
            code: 0,
            message: "incorrect inner message".into(),
            details: vec![prost_types::Any {
                type_url: READ_ERROR_TYPE.into(),
                value: detail.encode_to_vec(),
            }],
        };
        let error = ReadFailure {
            status: Status::with_details(
                failed.code,
                failed.message.clone(),
                rpc.encode_to_vec().into(),
            ),
            failed: Some(failed),
            unqueued: Vec::new(),
        }
        .into_status([4].into_iter());
        let rpc = RpcStatus::decode(error.details()).unwrap();
        assert_eq!(rpc.code, error.code() as i32);
        assert_eq!(rpc.message, error.message());
        let detail = pb::BidiReadObjectError::decode(rpc.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.read_range_errors.len(), 2);
    }

    #[test]
    fn bidi_range_bounds_and_ids() {
        let mut ranges = VecDeque::new();
        for (offset, length, expected) in [
            (-5, 3, (10, 3)),
            (i64::MIN, 0, (0, 15)),
            (15, 0, (15, 0)),
            (12, i64::MAX, (12, 3)),
        ] {
            submit(
                &mut ranges,
                vec![pb::ReadRange {
                    read_offset: offset,
                    read_length: length,
                    read_id: 1,
                }],
                15,
            )
            .unwrap();
            let range = ranges.pop_front().unwrap();
            assert_eq!((range.read_offset, range.read_length), expected);
        }
        for (offset, length) in [(16, 0), (0, -1)] {
            assert_code(
                submit(
                    &mut ranges,
                    vec![pb::ReadRange {
                        read_offset: offset,
                        read_length: length,
                        read_id: 1,
                    }],
                    15,
                ),
                tonic::Code::OutOfRange,
            );
        }
        let range = pb::ReadRange {
            read_id: 7,
            ..Default::default()
        };
        submit(&mut ranges, vec![range], 15).unwrap();
        assert_code(
            submit(&mut ranges, vec![range], 15),
            tonic::Code::InvalidArgument,
        );
        assert_code(
            submit(&mut ranges, vec![range; 101], 15),
            tonic::Code::InvalidArgument,
        );
        ranges.clear();
        submit(
            &mut ranges,
            (0..100)
                .map(|read_id| pb::ReadRange {
                    read_id,
                    ..Default::default()
                })
                .collect(),
            15,
        )
        .unwrap();
        submit(
            &mut ranges,
            (100..200)
                .map(|read_id| pb::ReadRange {
                    read_id,
                    ..Default::default()
                })
                .collect(),
            15,
        )
        .unwrap();
        assert_eq!(ranges.len(), 200);
    }
}

#[cfg(test)]
mod csek_tests;
#[cfg(test)]
mod mask_tests;
#[cfg(test)]
mod request_refusals;
