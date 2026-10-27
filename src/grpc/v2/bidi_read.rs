use super::{
    ResponseStream, Storage, convert, names, pb,
    status::{self, unsupported},
};
use crate::{
    auth::{self, Caller},
    bucket::BucketAccess,
    json::JsonError,
    object::{ObjectConditions, ObjectRecord},
    server::{State, anonymous_error, authorize_bucket},
    storage::ObjectReader,
};
use prost::Message;
use std::{
    collections::VecDeque,
    sync::{Arc, OnceLock, Weak},
};
use tokio::sync::{Mutex, mpsc};
use tonic::{Request, Response, Status, Streaming};

const MAX_RANGES: usize = 100;
const MAX_HANDLES: usize = 64;

struct Selected {
    record: ObjectRecord,
    reader: Mutex<ObjectReader>,
}

struct Handle {
    token: Vec<u8>,
    state: Weak<State>,
    project: Option<String>,
    selected: Arc<Selected>,
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

fn handles() -> &'static Mutex<VecDeque<Handle>> {
    static HANDLES: OnceLock<Mutex<VecDeque<Handle>>> = OnceLock::new();
    HANDLES.get_or_init(Mutex::default)
}

fn error(error: impl Into<JsonError>, caller: Option<Caller<'_>>) -> Status {
    status::from_json(anonymous_error(error.into(), caller))
}

#[allow(deprecated)]
fn validate_spec(spec: &pb::BidiReadObjectSpec) -> Result<(), Status> {
    if spec.generation < 0
        || [
            spec.if_generation_match,
            spec.if_generation_not_match,
            spec.if_metageneration_match,
            spec.if_metageneration_not_match,
        ]
        .into_iter()
        .flatten()
        .any(|value| value < 0)
    {
        return Err(Status::invalid_argument(
            "Generation conditions must be nonnegative; use generation 0 for the live object",
        ));
    }
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
        let access = authorize_bucket(&self.state, caller, auth::OBJECTS_GET, bucket)
            .await
            .map_err(|e| error(e, caller))?;
        let resumed = spec.read_handle.is_some();
        let (selected, token) = if let Some(handle) = spec.read_handle {
            let entries = handles().lock().await;
            let entry = entries
                .iter()
                .find(|entry| {
                    entry.token == handle.handle && entry.state.ptr_eq(&Arc::downgrade(&self.state))
                })
                .ok_or_else(|| {
                    Status::failed_precondition(
                        "Read handle is unknown or expired; reopen without read_handle",
                    )
                })?;
            check_handle_project(entry.project.as_deref(), &access)?;
            if entry.selected.record.bucket != bucket
                || entry.selected.record.name != object
                || (spec.generation != 0 && spec.generation != entry.selected.record.generation)
            {
                return Err(Status::invalid_argument(
                    "Read handle does not match bucket, object or generation; use the original object or reopen without read_handle",
                ));
            }
            (entry.selected.clone(), entry.token.clone())
        } else {
            let conditions = ObjectConditions {
                if_generation_match: spec.if_generation_match,
                if_generation_not_match: spec.if_generation_not_match,
                if_metageneration_match: spec.if_metageneration_match,
                if_metageneration_not_match: spec.if_metageneration_not_match,
            };
            let (record, reader) = self
                .state
                .catalog
                .open_object_authorized(
                    bucket,
                    object,
                    (spec.generation != 0).then_some(spec.generation),
                    conditions,
                    &access,
                )
                .await
                .map_err(|e| error(e, caller))?;
            let selected = Arc::new(Selected {
                record,
                reader: Mutex::new(reader),
            });
            let mut token = vec![0; 32];
            getrandom::fill(&mut token)
                .map_err(|_| Status::internal("Cannot create read handle; retry the read"))?;
            let mut entries = handles().lock().await;
            entries.retain(|entry| entry.state.strong_count() != 0);
            if entries.len() == MAX_HANDLES {
                entries.pop_front();
            }
            entries.push_back(Handle {
                token: token.clone(),
                state: Arc::downgrade(&self.state),
                project: project(&access).map(str::to_owned),
                selected: selected.clone(),
            });
            (selected, token)
        };
        let mut ranges = VecDeque::new();
        add_ranges(&mut ranges, first.read_ranges, selected.record.size)?;
        let initial = pb::BidiReadObjectResponse {
            metadata: if resumed {
                None
            } else {
                Some(convert::object(&selected.record)?)
            },
            read_handle: Some(pb::BidiReadHandle { handle: token }),
            ..Default::default()
        };
        let (tx, rx) = mpsc::channel(2);
        tokio::spawn(async move {
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
        let offset = if range.read_offset < 0 {
            size.saturating_sub(range.read_offset.unsigned_abs())
        } else {
            range.read_offset as u64
        };
        if offset > size || range.read_length < 0 {
            return Err(range_error(range.read_id));
        }
        let remaining = size - offset;
        range.read_offset = offset as i64;
        range.read_length = if range.read_length == 0 {
            remaining
        } else {
            remaining.min(range.read_length as u64)
        } as i64;
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
    let status = RpcStatus {
        code: tonic::Code::OutOfRange as i32,
        message: message.into(),
        details: vec![prost_types::Any {
            type_url: "type.googleapis.com/google.storage.v2.BidiReadObjectError".into(),
            value: details.encode_to_vec(),
        }],
    };
    Status::with_details(
        tonic::Code::OutOfRange,
        message,
        status.encode_to_vec().into(),
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
                    reader.set_range(offset as u64, range.read_length as u64).await
                        .map_err(|_| Status::internal("Cannot seek object data; restore a valid data directory"))?;
                    reader.next_chunk().await.map_err(|_| Status::internal("Cannot read object data; restore a valid data directory"))?
                        .unwrap_or_default()
                };
                range.read_offset += data.len() as i64;
                range.read_length -= data.len() as i64;
                let range_end = range.read_length == 0;
                let response = pb::BidiReadObjectResponse {
                    object_data_ranges: vec![pb::ObjectRangeData {
                        checksummed_data: Some(pb::ChecksummedData { crc32c: Some(crc32c::crc32c(&data)), content: data.to_vec() }),
                        read_range: Some(pb::ReadRange { read_offset: offset, read_length: data.len() as i64, read_id: range.read_id }),
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

    #[test]
    fn bidi_shared_error_status() {
        let unsupported = error(
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
            error(
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
        assert_eq!(
            check_handle_project(Some("original-project"), &access)
                .unwrap_err()
                .code(),
            tonic::Code::FailedPrecondition
        );
        assert!(check_handle_project(Some("new-project"), &access).is_ok());
        assert!(check_handle_project(None, &BucketAccess::Permissive).is_ok());
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
            assert_eq!(
                add_ranges(
                    &mut ranges,
                    vec![pb::ReadRange {
                        read_offset: offset,
                        read_length: length,
                        read_id: 1
                    }],
                    15
                )
                .unwrap_err()
                .code(),
                tonic::Code::OutOfRange
            );
        }
        let range = pb::ReadRange {
            read_id: 7,
            ..Default::default()
        };
        add_ranges(&mut ranges, vec![range], 15).unwrap();
        assert_eq!(
            add_ranges(&mut ranges, vec![range], 15).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            add_ranges(&mut ranges, vec![range; 101], 15)
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
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
