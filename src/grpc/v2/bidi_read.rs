use super::{ResponseStream, Storage, append::Finality, names, pb, status::unsupported};
use crate::{
    auth::{self, Caller},
    bucket::BucketAccess,
    grpc::validate::{self, request_conditions},
    json::JsonError,
    object::{ObjectRecord, ObjectTarget},
    storage::ObjectReader,
};
use prost::Message;
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::{Mutex, mpsc};
use tonic::{Request, Response, Status, Streaming};

const MAX_RANGES: usize = 100;
const MAX_HANDLES: usize = 64;

struct Selected {
    record: ObjectRecord,
    reader: Mutex<ObjectReader>,
    /// An appendable generation snapshot: reads stop at the size persisted when it was opened.
    finality: Finality,
}

struct Handle {
    token: Vec<u8>,
    project: Option<String>,
    selected: Arc<Selected>,
}

/// Read snapshots belong to one server state and survive individual gRPC connections.
#[derive(Default)]
pub struct ReadHandles(Mutex<VecDeque<Handle>>);

impl ReadHandles {
    async fn insert(&self, handle: Handle) {
        let mut entries = self.0.lock().await;
        if entries.len() == MAX_HANDLES {
            entries.pop_front();
        }
        entries.push_back(handle);
    }
}

fn project(access: &BucketAccess) -> Option<&str> {
    match access {
        BucketAccess::Policy { project, .. } => project.as_deref(),
        _ => None,
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

#[expect(
    deprecated,
    reason = "the deprecated read_mask field must still be rejected when a client sets it"
)]
fn validate_spec(spec: &pb::BidiReadObjectSpec) -> Result<(), Status> {
    const MESSAGE: &str =
        "Generation conditions must be nonnegative; use generation 0 for the live object";
    validate::generation(spec.generation, MESSAGE)?;
    validate::lenient_conditions(request_conditions!(spec), MESSAGE)?;
    if spec
        .common_object_request_params
        .as_ref()
        .is_some_and(|params| params != &Default::default())
        || spec
            .routing_token
            .as_ref()
            .is_some_and(|token| !token.is_empty())
        || spec
            .read_mask
            .as_ref()
            .is_some_and(|mask| !mask.paths.is_empty() && mask.paths != ["*"])
    {
        return Err(unsupported(
            "BidiReadObject encryption, routing_token or read_mask",
        ));
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
    Ok(())
}

impl Storage {
    async fn resume_read_handle(
        &self,
        spec: &pb::BidiReadObjectSpec,
        access: &BucketAccess,
        caller: Option<Caller<'_>>,
    ) -> Result<(Arc<Selected>, Vec<u8>), Status> {
        let handle = spec.read_handle.as_ref().ok_or_else(|| {
            Status::invalid_argument("Missing read_handle; reopen without read_handle")
        })?;
        let (bucket, object) = names::object_name(&spec.bucket, &spec.object)?;
        let entries = self.state.grpc_read_handles.0.lock().await;
        let entry = entries
            .iter()
            .find(|entry| entry.token == handle.handle)
            .ok_or_else(|| {
                Status::failed_precondition(
                    "Read handle is unknown or expired; reopen without read_handle",
                )
            })?;
        check_handle_project(entry.project.as_deref(), access)?;
        if entry.selected.record.bucket != bucket
            || entry.selected.record.name != object
            || (spec.generation != 0 && spec.generation != entry.selected.record.generation)
        {
            return Err(Status::invalid_argument(
                "Read handle does not match bucket, object or generation; use the original object or reopen without read_handle",
            ));
        }
        let selected = entry.selected.clone();
        let token = entry.token.clone();
        drop(entries);
        self.state
            .catalog
            .authorize_object_snapshot(&selected.record, access)
            .await
            .map_err(|failure| super::access_status(failure, caller))?;
        Ok((selected, token))
    }

    pub(super) async fn bidi_read_object(
        &self,
        request: Request<Streaming<pb::BidiReadObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::BidiReadObjectResponse>>, Status> {
        if request
            .metadata()
            .get("x-goog-fieldmask")
            .is_some_and(|mask| mask != "*")
        {
            return Err(unsupported("BidiReadObject x-goog-fieldmask"));
        }
        let mut input = request.into_inner();
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
        validate_spec(&spec)?;
        let (bucket, object) = names::object_name(&spec.bucket, &spec.object)?;
        let access = self.access(caller, bucket, auth::OBJECTS_GET).await?;
        let resumed = spec.read_handle.is_some();
        let (selected, token) = if resumed {
            self.resume_read_handle(&spec, &access, caller).await?
        } else {
            let conditions = request_conditions!(spec);
            let target = ObjectTarget::new(
                bucket,
                object,
                (spec.generation != 0).then_some(spec.generation),
            );
            let (record, reader, finality) = self
                .open_readable(target, conditions, &access)
                .await
                .map_err(|e| {
                    super::access_status(JsonError::from(e).naming_object(bucket, object), caller)
                })?;
            let selected = Arc::new(Selected {
                record,
                reader: Mutex::new(reader),
                finality,
            });
            let mut token = vec![0; 32];
            getrandom::fill(&mut token)
                .map_err(|_| Status::internal("Cannot create read handle; retry the read"))?;
            self.state
                .grpc_read_handles
                .insert(Handle {
                    token: token.clone(),
                    project: project(&access).map(str::to_owned),
                    selected: selected.clone(),
                })
                .await;
            (selected, token)
        };
        let mut ranges = VecDeque::new();
        add_ranges(&mut ranges, first.read_ranges, selected.record.size)?;
        let initial = pb::BidiReadObjectResponse {
            metadata: if resumed {
                None
            } else {
                Some(super::append::object(&selected.record, selected.finality)?)
            },
            read_handle: Some(pb::BidiReadHandle { handle: token }),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel(2);
        crate::task::spawn_supervised("grpc-bidi-read", async move {
            let work = async {
                tx.send(Ok(initial))
                    .await
                    .map_err(|_| Status::cancelled("Read stream closed"))?;
                serve(&mut input, &selected, &mut ranges, &tx).await
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

fn add_ranges(
    queue: &mut VecDeque<pb::ReadRange>,
    ranges: Vec<pb::ReadRange>,
    size: u64,
) -> Result<(), Status> {
    if ranges.len() > MAX_RANGES {
        return Err(Status::invalid_argument(
            "At most 100 ranges are allowed per message; split read_ranges across multiple messages",
        ));
    }
    for mut range in ranges {
        if queue.iter().any(|pending| pending.read_id == range.read_id) {
            return Err(Status::invalid_argument(
                "Duplicate outstanding read_id; assign a different read_id to each range",
            ));
        }
        let (offset, length) =
            crate::grpc::validate::resolve_range(size, range.read_offset, range.read_length)
                .ok_or_else(|| range_error(range.read_id))?;
        range.read_offset = crate::grpc::convert::signed(offset);
        range.read_length = crate::grpc::convert::signed(length);
        queue.push_back(range);
    }
    Ok(())
}

fn range_error(read_id: i64) -> Status {
    use crate::grpc::generated::google::rpc::Status as RpcStatus;
    let message = "Range exceeds object size or has negative read_length; use offset 0 and read_length 0 to read the object";
    let details = pb::BidiReadObjectError {
        read_range_errors: vec![pb::ReadRangeError {
            read_id,
            status: Some(RpcStatus {
                code: tonic::Code::OutOfRange as i32,
                message: message.into(),
                details: vec![],
            }),
        }],
    };
    super::status::with_detail(
        tonic::Code::OutOfRange,
        message.into(),
        "type.googleapis.com/google.storage.v2.BidiReadObjectError",
        details.encode_to_vec(),
    )
}

async fn serve(
    input: &mut Streaming<pb::BidiReadObjectRequest>,
    selected: &Selected,
    ranges: &mut VecDeque<pb::ReadRange>,
    tx: &mpsc::Sender<Result<pb::BidiReadObjectResponse, Status>>,
) -> Result<(), Status> {
    let mut eof = false;
    loop {
        if eof && ranges.is_empty() {
            return Ok(());
        }
        tokio::select! {
            message = input.message(), if !eof && ranges.len() < MAX_RANGES => {
                if let Some(message) = message? {
                    if message.read_object_spec.is_some() {
                        return Err(Status::invalid_argument("read_object_spec is only allowed in the first message; send only read_ranges afterward"));
                    }
                    add_ranges(ranges, message.read_ranges, selected.record.size)?;
                } else { eof = true; }
            }
            _ = std::future::ready(()), if !ranges.is_empty() => {
                let Some(mut range) = ranges.pop_front() else { continue };
                let offset = range.read_offset;
                let data = {
                    // Reposition under the same lock because reconnects share the immutable open file.
                    let mut reader = selected.reader.lock().await;
                    reader.set_range(offset.cast_unsigned(), range.read_length.cast_unsigned()).await
                        .map_err(|_| Status::internal("Cannot seek object data; restore a valid data directory"))?;
                    reader.next_chunk().await.map_err(|_| Status::internal("Cannot read object data; restore a valid data directory"))?
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
    use crate::storage::test_fixtures::PERMISSIVE;

    async fn readable_state() -> Arc<crate::server::State> {
        use crate::{
            auth::Auth,
            clock::SystemClock,
            storage::{Catalog, admin::LocalAdmin},
        };
        let catalog = Catalog::open_memory_for_server(
            1024 * 1024,
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

    #[tokio::test]
    async fn read_handles_are_isolated_by_state_and_survive_reconnection() {
        use crate::grpc::test_support::{rpc, storage_with};
        let states = [readable_state().await, readable_state().await];
        let mut selected = Vec::new();
        let mut handles = Vec::new();
        for state in &states {
            let request = pb::BidiReadObjectRequest {
                read_object_spec: Some(pb::BidiReadObjectSpec {
                    bucket: "projects/_/buckets/handle-bucket".into(),
                    object: "object".into(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let (code, responses): (_, Vec<pb::BidiReadObjectResponse>) = rpc(
                "/google.storage.v2.Storage/BidiReadObject",
                None,
                &[request],
                async |request| {
                    crate::grpc::v2::dispatch(
                        crate::grpc::v2::service(state.clone(), 1024),
                        request,
                    )
                    .await
                    .unwrap()
                },
            )
            .await;
            assert_eq!(code, tonic::Code::Ok);
            assert_eq!(responses.len(), 1);
            let token = responses[0].read_handle.as_ref().unwrap().handle.clone();
            let entries = state.grpc_read_handles.0.lock().await;
            selected.push(
                entries
                    .iter()
                    .find(|entry| entry.token == token)
                    .unwrap()
                    .selected
                    .clone(),
            );
            handles.push(token);
        }
        for token in 0..=MAX_HANDLES {
            states[1]
                .grpc_read_handles
                .insert(Handle {
                    token: token.to_le_bytes().to_vec(),
                    project: None,
                    selected: selected[1].clone(),
                })
                .await;
        }
        let retained: Vec<_> = states[1]
            .grpc_read_handles
            .0
            .lock()
            .await
            .iter()
            .map(|entry| entry.token.clone())
            .collect();
        assert_eq!(
            retained,
            (1..=MAX_HANDLES)
                .map(|token| token.to_le_bytes().to_vec())
                .collect::<Vec<_>>()
        );
        let spec = pb::BidiReadObjectSpec {
            bucket: "projects/_/buckets/handle-bucket".into(),
            object: "object".into(),
            read_handle: Some(pb::BidiReadHandle {
                handle: handles[0].clone(),
            }),
            ..Default::default()
        };
        let reconnected = storage_with(states[0].clone(), 1024);
        let (resumed, token) = reconnected
            .resume_read_handle(&spec, PERMISSIVE, None)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&resumed, &selected[0]));
        assert_eq!(token, handles[0]);
        assert_eq!(
            resumed
                .reader
                .lock()
                .await
                .next_chunk()
                .await
                .unwrap()
                .unwrap(),
            b"handle data".as_slice()
        );
        let other = storage_with(states[1].clone(), 1024);
        assert_code(
            other.resume_read_handle(&spec, PERMISSIVE, None).await,
            tonic::Code::FailedPrecondition,
        );
        drop(resumed);
        drop(selected);
        for state in states {
            state.catalog.close().await.unwrap();
        }
    }

    #[test]
    fn bidi_shared_error_status() {
        let unsupported = access_status(
            JsonError::new(
                hyper::StatusCode::NOT_IMPLEMENTED,
                "notImplemented",
                "Unsupported setting",
            ),
            None,
        );
        assert_eq!(unsupported.code(), tonic::Code::Unimplemented);
        assert!(!unsupported.details().is_empty());
        assert_eq!(
            access_status(
                JsonError::new(
                    hyper::StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Storage unavailable"
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
    fn bidi_range_bounds_and_ids() {
        let mut ranges = VecDeque::new();
        for (offset, length, expected) in [
            (-5, 3, (10, 3)),
            (i64::MIN, 0, (0, 15)),
            (15, 0, (15, 0)),
            (12, i64::MAX, (12, 3)),
        ] {
            add_ranges(
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
                add_ranges(
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
        add_ranges(&mut ranges, vec![range], 15).unwrap();
        assert_code(
            add_ranges(&mut ranges, vec![range], 15),
            tonic::Code::InvalidArgument,
        );
        assert_code(
            add_ranges(&mut ranges, vec![range; 101], 15),
            tonic::Code::InvalidArgument,
        );
        ranges.clear();
        add_ranges(
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
        add_ranges(
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
