//! Pins, for WriteObject and BidiWriteObject, the status and message of every per-message error
//! of single-shot, resumable and appendable writes, which error wins when one message has
//! several, the replies sent before it, and what the write leaves behind. Each case goes through
//! the RPC service as a client sends it. The pinned proto names a status only for an object_size
//! mismatch (OUT_OF_RANGE), so the other codes and the precedence are the emulator's.

use super::super::tests::{HeldOpen, Hold, setup, spec};
use super::*;
use crate::grpc::test_support::{key_params, rpc_status};
use bytes::Bytes;
use pb::bidi_write_object_request::{Data, FirstMessage};
use tonic::Code;

/// The largest object the service under test accepts, small enough to exceed in one message.
const MAX: u64 = 16;
const BUCKET: &str = "projects/_/buckets/grpc-io";

const FIRST: &str = "First message must contain write_object_spec or upload_id";
const NO_FINISH: &str = "Non-resumable write requires finish_write=true on its final message";
const AFTER_FINISH: &str =
    "A message followed finish_write; finish_write must be the final message";
const LATER_SPEC: &str = "Only the first message may contain write_object_spec";
const LATER_UPLOAD_ID: &str = "Only the first message may contain write_object_spec or upload_id";
const LATER_APPEND_SPEC: &str =
    "Only the first message may contain write_object_spec or append_object_spec";
const LATER_CHECKSUMS: &str = "object_checksums is allowed only in the first or final message";
const MISMATCH: &str = "object_checksums mismatch; send checksums for the complete object";
const CONFLICTING: &str = "Conflicting object_checksums; use the same digest in each message";
const MD5_LENGTH: &str = "md5_hash must contain exactly 16 bytes";
const NEGATIVE: &str = "write_offset must be nonnegative; use 0 for the first message";
const RESUME_NEGATIVE: &str = "write_offset must be nonnegative; query the persisted offset";
const APPEND_NEGATIVE: &str = "write_offset must be nonnegative; resume at persisted_size";
const RECEIVED_OFFSET: &str =
    "write_offset exceeds received bytes; start at 0 and continue at the received byte count";
const RESUME_OFFSET: &str = "write_offset does not match persisted data; call QueryWriteStatus and resume at persisted_size";
const SIZE: &str =
    "Written bytes do not match object_size; start a new upload with the correct size";
const TOO_LARGE: &str =
    "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object";
const NO_SESSION: &str = "Upload session not found; start a new resumable write";
const UNKNOWN_UPLOAD_ID: &str = "Unknown upload_id; use the value returned by StartResumableWrite";
const APPEND_CHECKSUMS: &str =
    "Appendable objects accept object_checksums only with finish_write=true";
const APPEND_MD5: &str =
    "Appendable objects have no MD5 hash; send only crc32c in object_checksums";
const APPEND_KEY: &str = "gcs-mimic does not support customer-supplied encryption on appendable objects; omit common_object_request_params";
const OPENING_GAP: &str =
    "write_offset leaves a gap after the 0 bytes received; resume at persisted_size";
const NON_RESUMABLE_FLUSH: &str =
    "non-resumable flush/state_lookup; use StartResumableWrite and upload_id";
const RESUMABLE_CHECKSUMS: &str = "object_checksums before resumable finish; set StartResumableWrite.object_checksums or send them with finish_write=true";
/// The shared catalog's wording when the complete object `abc` fails a declared CRC32C of 1.
/// Resumable and appendable writes check the complete object there, single-shot writes in the
/// adapter.
const STORED_MISMATCH: &str =
    "Provided CRC32C \"AAAAAQ==\" doesn't match calculated CRC32C \"Nks/tw==\".";

/// The UNIMPLEMENTED wording for a gRPC feature the emulator refuses.
fn unsupported(feature: &str) -> String {
    format!(
        "gcs-mimic does not support gRPC {feature}; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
    )
}

fn crc_mismatch(content: &[u8]) -> String {
    format!(
        "Checksummed data message crc32c mismatch: server computed: '{:08x}', client provided '00000001'.",
        crc32c::crc32c(content)
    )
}

fn key_after_keyless() -> String {
    super::super::super::status::customer_encryption(CsekError::ResourceNotEncrypted)
        .message()
        .to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rpc {
    WriteObject,
    Bidi,
}

/// A write status as the client receives it, with the object size a resource carries.
#[derive(Debug, PartialEq, Eq)]
enum Reply {
    Persisted(i64),
    Resource(i64),
}

/// What a write leaves behind: a readable object of some size, or a resumable session's state.
#[derive(Debug, PartialEq, Eq)]
enum Left {
    Nothing,
    Object(i64),
    Persisted(i64),
    Finalized,
    SessionGone,
}

#[derive(Debug, PartialEq, Eq)]
struct Answer {
    code: Code,
    message: String,
    replies: Vec<Reply>,
    left: Left,
}

fn ok(replies: Vec<Reply>, left: Left) -> Answer {
    Answer {
        code: Code::Ok,
        message: String::new(),
        replies,
        left,
    }
}

fn failed(code: Code, message: impl Into<String>, left: Left) -> Answer {
    Answer {
        code,
        message: message.into(),
        replies: Vec::new(),
        left,
    }
}

impl Answer {
    /// The same failure, after the stream answered `replies`.
    fn after(mut self, replies: Vec<Reply>) -> Self {
        self.replies = replies;
        self
    }
}

fn data(content: &[u8]) -> Option<Data> {
    Some(Data::ChecksummedData(pb::ChecksummedData {
        content: Bytes::copy_from_slice(content),
        crc32c: Some(crc32c::crc32c(content)),
    }))
}

/// A message carrying `content` at `offset`, with no first message and no finish_write.
fn chunk(offset: i64, content: &[u8]) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        write_offset: offset,
        data: data(content),
        ..Default::default()
    }
}

/// The first message of a single-shot write of `content`, without finish_write.
fn opening(name: &str, content: &[u8]) -> pb::BidiWriteObjectRequest {
    chunk(0, content).first(FirstMessage::WriteObjectSpec(spec(name)))
}

/// [`opening`] declaring `size` as the object's size.
fn sized(name: &str, size: i64, content: &[u8]) -> pb::BidiWriteObjectRequest {
    let mut spec = spec(name);
    spec.object_size = Some(size);
    chunk(0, content).first(FirstMessage::WriteObjectSpec(spec))
}

/// The first message of a stream that resumes `upload_id`, without finish_write.
fn resume(upload_id: &str, offset: i64, content: &[u8]) -> pb::BidiWriteObjectRequest {
    chunk(offset, content).first(FirstMessage::UploadId(upload_id.into()))
}

/// The first message of a stream that creates the appendable object `name`.
fn appendable(name: &str, content: &[u8]) -> pb::BidiWriteObjectRequest {
    let mut spec = spec(name);
    spec.appendable = Some(true);
    chunk(0, content).first(FirstMessage::WriteObjectSpec(spec))
}

/// Edits a test message in place of a struct literal per case.
trait Edit {
    fn first(self, first: FirstMessage) -> Self;
    fn at(self, offset: i64) -> Self;
    fn finish(self) -> Self;
    fn flush(self) -> Self;
    fn lookup(self) -> Self;
    fn crc(self, crc32c: u32) -> Self;
    fn md5(self, length: usize) -> Self;
    fn bad_crc(self) -> Self;
    fn keyed(self) -> Self;
}

impl Edit for pb::BidiWriteObjectRequest {
    fn first(mut self, first: FirstMessage) -> Self {
        self.first_message = Some(first);
        self
    }
    fn at(mut self, offset: i64) -> Self {
        self.write_offset = offset;
        self
    }
    fn finish(mut self) -> Self {
        self.finish_write = true;
        self
    }
    fn flush(mut self) -> Self {
        self.flush = true;
        self
    }
    fn lookup(mut self) -> Self {
        self.state_lookup = true;
        self
    }
    fn crc(mut self, crc32c: u32) -> Self {
        self.object_checksums = Some(pb::ObjectChecksums {
            crc32c: Some(crc32c),
            md5_hash: Vec::new(),
        });
        self
    }
    fn md5(mut self, length: usize) -> Self {
        self.object_checksums = Some(pb::ObjectChecksums {
            crc32c: None,
            md5_hash: vec![0; length],
        });
        self
    }
    fn bad_crc(mut self) -> Self {
        if let Some(Data::ChecksummedData(data)) = self.data.as_mut() {
            data.crc32c = Some(1);
        }
        self
    }
    fn keyed(mut self) -> Self {
        self.common_object_request_params = key_params(&[1; 32]);
        self
    }
}

/// `message` as WriteObject sends it; WriteObject has no flush, state_lookup or append spec.
fn write_object_request(message: &pb::BidiWriteObjectRequest) -> pb::WriteObjectRequest {
    use pb::write_object_request as write;
    assert!(!message.flush && !message.state_lookup);
    pb::WriteObjectRequest {
        first_message: message.first_message.clone().map(|first| match first {
            FirstMessage::UploadId(id) => write::FirstMessage::UploadId(id),
            FirstMessage::WriteObjectSpec(spec) => write::FirstMessage::WriteObjectSpec(spec),
            FirstMessage::AppendObjectSpec(_) => panic!("WriteObject has no append_object_spec"),
        }),
        write_offset: message.write_offset,
        data: message
            .data
            .clone()
            .map(|Data::ChecksummedData(data)| write::Data::ChecksummedData(data)),
        object_checksums: message.object_checksums.clone(),
        finish_write: message.finish_write,
        common_object_request_params: message.common_object_request_params.clone(),
    }
}

fn reply(status: pb::bidi_write_object_response::WriteStatus) -> Reply {
    use pb::bidi_write_object_response::WriteStatus;
    match status {
        WriteStatus::PersistedSize(size) => Reply::Persisted(size),
        WriteStatus::Resource(object) => Reply::Resource(object.size),
    }
}

/// Sends `messages` over `rpc` and returns its status and the write statuses it answered.
async fn call(
    storage: &Storage,
    rpc: Rpc,
    messages: &[pb::BidiWriteObjectRequest],
) -> (Status, Vec<Reply>) {
    let send = async |request| {
        crate::grpc::v2::dispatch(
            crate::grpc::v2::service(storage.state.clone(), MAX, storage.compat_profile),
            request,
        )
        .await
        .unwrap()
    };
    match rpc {
        Rpc::WriteObject => {
            let requests: Vec<_> = messages.iter().map(write_object_request).collect();
            let (status, responses): (_, Vec<pb::WriteObjectResponse>) = rpc_status(
                "/google.storage.v2.Storage/WriteObject",
                None,
                &requests,
                send,
            )
            .await;
            let replies = responses
                .into_iter()
                .filter_map(|response| response.write_status)
                .map(|status| {
                    use pb::write_object_response::WriteStatus;
                    match status {
                        WriteStatus::PersistedSize(size) => Reply::Persisted(size),
                        WriteStatus::Resource(object) => Reply::Resource(object.size),
                    }
                })
                .collect();
            (status, replies)
        }
        Rpc::Bidi => {
            let (status, responses): (_, Vec<pb::BidiWriteObjectResponse>) = rpc_status(
                "/google.storage.v2.Storage/BidiWriteObject",
                None,
                messages,
                send,
            )
            .await;
            let replies = responses
                .into_iter()
                .filter_map(|response| response.write_status)
                .map(reply)
                .collect();
            (status, replies)
        }
    }
}

async fn object_left(storage: &Storage, name: &str) -> Left {
    let request = pb::GetObjectRequest {
        bucket: BUCKET.into(),
        object: name.into(),
        ..Default::default()
    };
    match storage.get_object(Request::new(request), None).await {
        Ok(object) => Left::Object(object.into_inner().size),
        Err(status) if status.code() == Code::NotFound => Left::Nothing,
        Err(status) => panic!("{name}: {status}"),
    }
}

async fn query_status(
    storage: &Storage,
    upload_id: &str,
) -> Result<pb::QueryWriteStatusResponse, Status> {
    let request = pb::QueryWriteStatusRequest {
        upload_id: upload_id.into(),
        ..Default::default()
    };
    storage
        .query_write_status(Request::new(request), None)
        .await
        .map(Response::into_inner)
}

async fn session_left(storage: &Storage, upload_id: &str) -> Left {
    use pb::query_write_status_response::WriteStatus;
    match query_status(storage, upload_id).await {
        Ok(response) => match response.write_status {
            Some(WriteStatus::PersistedSize(size)) => Left::Persisted(size),
            Some(WriteStatus::Resource(_)) => Left::Finalized,
            None => panic!("{upload_id}: no write status"),
        },
        Err(status) if status.code() == Code::NotFound => Left::SessionGone,
        Err(status) => panic!("{upload_id}: {status}"),
    }
}

fn answer(status: &Status, replies: Vec<Reply>, left: Left) -> Answer {
    Answer {
        code: status.code(),
        message: status.message().to_owned(),
        replies,
        left,
    }
}

/// One stream's messages, built for the object or upload a case gets to itself.
type Messages = fn(&str) -> Vec<pb::BidiWriteObjectRequest>;

/// A case and what each RPC answers; `None` where WriteObject cannot send the messages.
struct Case {
    label: &'static str,
    messages: Messages,
    write_object: Option<Answer>,
    bidi: Answer,
}

fn both(label: &'static str, messages: Messages, answer: fn() -> Answer) -> Case {
    Case {
        label,
        messages,
        write_object: Some(answer()),
        bidi: answer(),
    }
}

fn bidi(label: &'static str, messages: Messages, answer: Answer) -> Case {
    Case {
        label,
        messages,
        write_object: None,
        bidi: answer,
    }
}

fn split(label: &'static str, messages: Messages, write_object: Answer, bidi: Answer) -> Case {
    Case {
        label,
        messages,
        write_object: Some(write_object),
        bidi,
    }
}

/// Runs each single-shot or appendable case on an object of its own per RPC.
async fn run_on_objects(storage: &Storage, cases: Vec<Case>) {
    for case in cases {
        for (rpc, expected) in [
            (Rpc::WriteObject, case.write_object),
            (Rpc::Bidi, Some(case.bidi)),
        ] {
            let Some(expected) = expected else { continue };
            let name = format!("{}-{rpc:?}", case.label);
            let (status, replies) = call(storage, rpc, &(case.messages)(&name)).await;
            let actual = answer(&status, replies, object_left(storage, &name).await);
            assert_eq!(actual, expected, "{} over {rpc:?}", case.label);
        }
    }
}

#[tokio::test]
async fn grpc_single_shot_write_message_errors() {
    use Code::{InvalidArgument, OutOfRange, ResourceExhausted, Unimplemented};
    let (_dir, storage) = setup(false).await;
    let flush = unsupported(NON_RESUMABLE_FLUSH);
    let cases = vec![
        both(
            "two-messages",
            |n| vec![opening(n, b"abc"), chunk(3, b"def").finish()],
            || ok(vec![Reply::Resource(6)], Left::Object(6)),
        ),
        both(
            "empty-stream",
            |_| vec![],
            || failed(InvalidArgument, FIRST, Left::Nothing),
        ),
        both(
            "no-first-message",
            |_| vec![chunk(0, b"abc").finish()],
            || failed(InvalidArgument, FIRST, Left::Nothing),
        ),
        both(
            "no-finish",
            |n| vec![opening(n, b"abc")],
            || failed(InvalidArgument, NO_FINISH, Left::Nothing),
        ),
        // The proto: "Sending any WriteObjectRequests subsequent to one in which finish_write
        // is true causes an error." BidiWriteObject answers at finish_write and reads no
        // further, so the later message is never seen.
        split(
            "after-finish",
            |n| vec![opening(n, b"abc").finish(), chunk(3, b"def")],
            failed(InvalidArgument, AFTER_FINISH, Left::Nothing),
            ok(vec![Reply::Resource(3)], Left::Object(3)),
        ),
        bidi(
            "flush",
            |n| vec![opening(n, b"abc").flush(), chunk(3, b"d").finish()],
            failed(Unimplemented, flush.clone(), Left::Nothing),
        ),
        bidi(
            "state-lookup",
            |n| vec![opening(n, b"abc").lookup(), chunk(3, b"d").finish()],
            failed(Unimplemented, flush.clone(), Left::Nothing),
        ),
        // The proto ignores flush and state_lookup on a finish_write message.
        bidi(
            "finish-with-flush",
            |n| vec![opening(n, b"abc").finish().flush().lookup()],
            ok(vec![Reply::Resource(3)], Left::Object(3)),
        ),
        both(
            "later-spec",
            |n| {
                let spec = FirstMessage::WriteObjectSpec(spec(n));
                vec![opening(n, b"abc"), chunk(3, b"d").finish().first(spec)]
            },
            || failed(InvalidArgument, LATER_SPEC, Left::Nothing),
        ),
        both(
            "later-upload-id",
            |n| {
                let id = FirstMessage::UploadId("grpc-io/upload".into());
                vec![opening(n, b"abc"), chunk(3, b"d").finish().first(id)]
            },
            || failed(InvalidArgument, LATER_SPEC, Left::Nothing),
        ),
        both(
            "later-checksums",
            |n| {
                vec![
                    opening(n, b"abc"),
                    chunk(3, b"d").crc(1),
                    chunk(4, b"").finish(),
                ]
            },
            || failed(InvalidArgument, LATER_CHECKSUMS, Left::Nothing),
        ),
        both(
            "final-checksums",
            |n| {
                let whole = crc32c::crc32c(b"abcdef");
                vec![opening(n, b"abc"), chunk(3, b"def").finish().crc(whole)]
            },
            || ok(vec![Reply::Resource(6)], Left::Object(6)),
        ),
        both(
            "final-checksum-mismatch",
            |n| vec![opening(n, b"abc"), chunk(3, b"def").finish().crc(1)],
            || failed(InvalidArgument, MISMATCH, Left::Nothing),
        ),
        both(
            "conflicting-checksums",
            |n| {
                let whole = crc32c::crc32c(b"abcdef");
                vec![
                    opening(n, b"abc").crc(whole),
                    chunk(3, b"def").finish().crc(1),
                ]
            },
            || failed(InvalidArgument, CONFLICTING, Left::Nothing),
        ),
        both(
            "first-md5-length",
            |n| vec![opening(n, b"abc").finish().md5(3)],
            || failed(InvalidArgument, MD5_LENGTH, Left::Nothing),
        ),
        both(
            "final-md5-length",
            |n| vec![opening(n, b"abc"), chunk(3, b"def").finish().md5(3)],
            || failed(InvalidArgument, MD5_LENGTH, Left::Nothing),
        ),
        both(
            "negative-offset",
            |n| {
                vec![
                    chunk(-1, b"abc")
                        .finish()
                        .first(FirstMessage::WriteObjectSpec(spec(n))),
                ]
            },
            || failed(OutOfRange, NEGATIVE, Left::Nothing),
        ),
        both(
            "first-offset",
            |n| {
                vec![
                    chunk(2, b"abc")
                        .finish()
                        .first(FirstMessage::WriteObjectSpec(spec(n))),
                ]
            },
            || failed(OutOfRange, RECEIVED_OFFSET, Left::Nothing),
        ),
        both(
            "gap",
            |n| vec![opening(n, b"abc"), chunk(5, b"def").finish()],
            || failed(OutOfRange, RECEIVED_OFFSET, Left::Nothing),
        ),
        // A resent prefix is skipped, as the proto's "no larger than" rule allows.
        both(
            "overlap",
            |n| vec![opening(n, b"abc"), chunk(1, b"bcdef").finish()],
            || ok(vec![Reply::Resource(6)], Left::Object(6)),
        ),
        both(
            "bad-crc",
            |n| vec![opening(n, b"abc").finish().bad_crc()],
            || failed(InvalidArgument, crc_mismatch(b"abc"), Left::Nothing),
        ),
        both(
            "object-size-short",
            |n| vec![sized(n, 5, b"abc").finish()],
            || failed(OutOfRange, SIZE, Left::Nothing),
        ),
        both(
            "object-size-over",
            |n| vec![sized(n, 2, b"abc")],
            || failed(OutOfRange, SIZE, Left::Nothing),
        ),
        both(
            "object-size-negative",
            |n| vec![sized(n, -1, b"").finish()],
            || {
                failed(
                    InvalidArgument,
                    "object_size must be nonnegative",
                    Left::Nothing,
                )
            },
        ),
        both(
            "object-size-above-max",
            |n| vec![sized(n, 17, b"").finish()],
            || {
                failed(
                    ResourceExhausted,
                    "object_size exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
                    Left::Nothing,
                )
            },
        ),
        both(
            "too-large",
            |n| vec![opening(n, &[0; 17]).finish()],
            || failed(ResourceExhausted, TOO_LARGE, Left::Nothing),
        ),
        both(
            "key-after-keyless",
            |n| vec![opening(n, b"abc"), chunk(3, b"d").finish().keyed()],
            || failed(InvalidArgument, key_after_keyless(), Left::Nothing),
        ),
    ];
    run_on_objects(&storage, cases).await;
    storage.state.catalog.close().await.unwrap();
}

/// When one message breaks several rules, the first rule checked decides the answer.
#[tokio::test]
async fn grpc_single_shot_write_error_precedence() {
    use Code::{FailedPrecondition, InvalidArgument, OutOfRange, Unimplemented};
    let (_dir, storage) = setup(false).await;
    let flush = unsupported(NON_RESUMABLE_FLUSH);
    let cases = vec![
        both(
            "key-over-later-spec",
            |n| {
                let spec = FirstMessage::WriteObjectSpec(spec(n));
                vec![
                    opening(n, b"abc"),
                    chunk(3, b"d").finish().keyed().first(spec),
                ]
            },
            || failed(InvalidArgument, key_after_keyless(), Left::Nothing),
        ),
        bidi(
            "later-spec-over-flush",
            |n| {
                let spec = FirstMessage::WriteObjectSpec(spec(n));
                vec![opening(n, b"abc"), chunk(3, b"d").flush().first(spec)]
            },
            failed(InvalidArgument, LATER_SPEC, Left::Nothing),
        ),
        bidi(
            "flush-over-later-checksums",
            |n| vec![opening(n, b"abc"), chunk(3, b"d").flush().crc(1)],
            failed(Unimplemented, flush.clone(), Left::Nothing),
        ),
        both(
            "later-checksums-over-offset",
            |n| vec![opening(n, b"abc"), chunk(-1, b"d").crc(1)],
            || failed(InvalidArgument, LATER_CHECKSUMS, Left::Nothing),
        ),
        both(
            "md5-length-over-offset",
            |n| vec![opening(n, b"abc"), chunk(-1, b"d").finish().md5(3)],
            || failed(InvalidArgument, MD5_LENGTH, Left::Nothing),
        ),
        both(
            "conflicting-checksums-over-offset",
            |n| {
                let whole = crc32c::crc32c(b"abcd");
                vec![
                    opening(n, b"abc").crc(whole),
                    chunk(-1, b"d").finish().crc(1),
                ]
            },
            || failed(InvalidArgument, CONFLICTING, Left::Nothing),
        ),
        both(
            "negative-offset-over-crc",
            |n| {
                vec![
                    chunk(-1, b"abc")
                        .finish()
                        .bad_crc()
                        .first(FirstMessage::WriteObjectSpec(spec(n))),
                ]
            },
            || failed(OutOfRange, NEGATIVE, Left::Nothing),
        ),
        both(
            "gap-over-crc",
            |n| vec![opening(n, b"abc"), chunk(5, b"def").finish().bad_crc()],
            || failed(OutOfRange, RECEIVED_OFFSET, Left::Nothing),
        ),
        both(
            "crc-over-object-size",
            |n| vec![sized(n, 2, b"abc").finish().bad_crc()],
            || failed(InvalidArgument, crc_mismatch(b"abc"), Left::Nothing),
        ),
        both(
            "crc-over-max",
            |n| vec![opening(n, &[0; 17]).finish().bad_crc()],
            || failed(InvalidArgument, crc_mismatch(&[0; 17]), Left::Nothing),
        ),
        // The spec is checked, and the destination prepared, before the first message's data.
        both(
            "spec-over-message",
            |n| {
                let mut spec = spec(n);
                spec.predefined_acl = "private".into();
                vec![
                    chunk(-1, b"abc")
                        .bad_crc()
                        .first(FirstMessage::WriteObjectSpec(spec)),
                ]
            },
            || {
                failed(
                    Unimplemented,
                    unsupported("appendable/predefined_acl; omit these fields"),
                    Left::Nothing,
                )
            },
        ),
        bidi(
            "precondition-over-flush",
            |n| {
                let mut spec = spec(n);
                spec.if_generation_match = Some(5);
                vec![
                    chunk(0, b"abc")
                        .flush()
                        .first(FirstMessage::WriteObjectSpec(spec)),
                ]
            },
            failed(FailedPrecondition, PRECONDITION, Left::Nothing),
        ),
        // WriteObject reads past finish_write before it checks the complete object.
        split(
            "later-message-over-object-checksums",
            |n| vec![opening(n, b"abc").finish().crc(1), chunk(3, b"d")],
            failed(InvalidArgument, AFTER_FINISH, Left::Nothing),
            failed(InvalidArgument, MISMATCH, Left::Nothing),
        ),
    ];
    run_on_objects(&storage, cases).await;
    storage.state.catalog.close().await.unwrap();
}

/// How a case's resumable session starts: its declared size and checksum, the bytes persisted
/// before the case, whether a stream already finalized it with them, and whether another write
/// created the object after a session that may only create it started.
#[derive(Clone, Copy, Default)]
struct Session {
    size: Option<i64>,
    crc32c: Option<u32>,
    prefix: &'static [u8],
    finalized: bool,
    preempted: bool,
}

const PLAIN: Session = Session {
    size: None,
    crc32c: None,
    prefix: b"",
    finalized: false,
    preempted: false,
};

const PREFIXED: Session = Session {
    prefix: b"abc",
    ..PLAIN
};

/// A create-only session whose object another write created, so its publication conditions
/// no longer hold.
const PREEMPTED: Session = Session {
    preempted: true,
    ..PLAIN
};

const PRECONDITION: &str = "At least one of the pre-conditions you specified did not hold.";

async fn open_session(storage: &Storage, name: &str, session: Session) -> String {
    let mut spec = spec(name);
    spec.object_size = session.size;
    if session.preempted {
        spec.if_generation_match = Some(0);
    }
    let upload = storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(spec),
                object_checksums: session.crc32c.map(|crc32c| pb::ObjectChecksums {
                    crc32c: Some(crc32c),
                    md5_hash: Vec::new(),
                }),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner()
        .upload_id;
    if !session.prefix.is_empty() || session.finalized {
        let mut prefix = resume(&upload, 0, session.prefix);
        prefix.finish_write = session.finalized;
        let (status, _) = call(storage, Rpc::WriteObject, &[prefix]).await;
        assert_eq!(status.code(), Code::Ok, "{name}: {status}");
    }
    if session.preempted {
        let (status, _) = call(storage, Rpc::WriteObject, &[opening(name, b"x").finish()]).await;
        assert_eq!(status.code(), Code::Ok, "{name}: {status}");
    }
    upload
}

/// A resumable case: how its session starts and what each RPC answers.
struct ResumableCase {
    session: Session,
    case: Case,
}

fn on(session: Session, case: Case) -> ResumableCase {
    ResumableCase { session, case }
}

/// Runs each resumable case on a session of its own per RPC.
async fn run_on_sessions(storage: &Storage, cases: Vec<ResumableCase>) {
    for ResumableCase { session, case } in cases {
        for (rpc, expected) in [
            (Rpc::WriteObject, case.write_object),
            (Rpc::Bidi, Some(case.bidi)),
        ] {
            let Some(expected) = expected else { continue };
            let name = format!("{}-{rpc:?}", case.label);
            let upload = open_session(storage, &name, session).await;
            let (status, replies) = call(storage, rpc, &(case.messages)(&upload)).await;
            let actual = answer(&status, replies, session_left(storage, &upload).await);
            assert_eq!(actual, expected, "{} over {rpc:?}", case.label);
        }
    }
}

/// An upload ID of the issued shape that names no session.
fn unknown(_: &str) -> String {
    format!("grpc-io/{}", "0".repeat(64))
}

#[tokio::test]
async fn grpc_resumable_write_message_errors() {
    use Code::{InvalidArgument, NotFound, OutOfRange, ResourceExhausted, Unimplemented};
    let (_dir, storage) = setup(false).await;
    let cases = vec![
        on(
            PLAIN,
            both(
                "finish",
                |u| vec![resume(u, 0, b"abc"), chunk(3, b"def").finish()],
                || ok(vec![Reply::Resource(6)], Left::Finalized),
            ),
        ),
        on(
            PLAIN,
            both(
                "half-close",
                |u| vec![resume(u, 0, b"abc")],
                || ok(vec![Reply::Persisted(3)], Left::Persisted(3)),
            ),
        ),
        // Each flush or state_lookup is answered, and so is a half-close, as the proto says:
        // "When the stream is closed, a BidiWriteObjectResponse is always sent to the client,
        // regardless of the value of state_lookup."
        on(
            PLAIN,
            bidi(
                "lookup-then-close",
                |u| vec![resume(u, 0, b"abc").lookup()],
                ok(
                    vec![Reply::Persisted(3), Reply::Persisted(3)],
                    Left::Persisted(3),
                ),
            ),
        ),
        on(
            PLAIN,
            bidi(
                "flush-then-close",
                |u| vec![resume(u, 0, b"abc"), chunk(3, b"d").flush()],
                ok(
                    vec![Reply::Persisted(4), Reply::Persisted(4)],
                    Left::Persisted(4),
                ),
            ),
        ),
        on(
            PLAIN,
            bidi(
                "flush-and-lookup",
                |u| {
                    vec![
                        resume(u, 0, b"abc").flush(),
                        chunk(3, b"def").lookup(),
                        chunk(6, b"g"),
                    ]
                },
                ok(
                    vec![
                        Reply::Persisted(3),
                        Reply::Persisted(6),
                        Reply::Persisted(7),
                    ],
                    Left::Persisted(7),
                ),
            ),
        ),
        on(
            PLAIN,
            bidi(
                "finish-with-flush",
                |u| vec![resume(u, 0, b"abc").finish().flush().lookup()],
                ok(vec![Reply::Resource(3)], Left::Finalized),
            ),
        ),
        on(
            PLAIN,
            both(
                "later-upload-id",
                |u| {
                    vec![
                        resume(u, 0, b"abc"),
                        chunk(3, b"d")
                            .finish()
                            .first(FirstMessage::UploadId(u.into())),
                    ]
                },
                || failed(InvalidArgument, LATER_UPLOAD_ID, Left::Persisted(3)),
            ),
        ),
        on(
            PLAIN,
            both(
                "checksums-before-finish",
                |u| vec![resume(u, 0, b"abc").crc(1)],
                || {
                    failed(
                        Unimplemented,
                        unsupported(RESUMABLE_CHECKSUMS),
                        Left::Persisted(0),
                    )
                },
            ),
        ),
        on(
            PLAIN,
            both(
                "later-checksums-before-finish",
                |u| vec![resume(u, 0, b"abc"), chunk(3, b"d").crc(1)],
                || {
                    failed(
                        Unimplemented,
                        unsupported(RESUMABLE_CHECKSUMS),
                        Left::Persisted(3),
                    )
                },
            ),
        ),
        on(
            PLAIN,
            both(
                "negative-offset",
                |u| vec![resume(u, -1, b"abc").finish()],
                || failed(OutOfRange, RESUME_NEGATIVE, Left::Persisted(0)),
            ),
        ),
        // The proto contradicts itself: the write_offset field says the first value "must be
        // equal to the persisted_size that a call to QueryWriteStatus() would return", while the
        // WriteObject description lets a client start earlier and skips the persisted bytes. The
        // emulator keeps the field rule, an owner decision, so a first message cannot resend
        // persisted bytes.
        on(
            PREFIXED,
            both(
                "first-offset-behind",
                |u| vec![resume(u, 1, b"bcdef").finish()],
                || failed(OutOfRange, RESUME_OFFSET, Left::Persisted(3)),
            ),
        ),
        on(
            PREFIXED,
            both(
                "first-offset-ahead",
                |u| vec![resume(u, 5, b"f").finish()],
                || failed(OutOfRange, RESUME_OFFSET, Left::Persisted(3)),
            ),
        ),
        on(
            PLAIN,
            both(
                "later-offset-ahead",
                |u| vec![resume(u, 0, b"abc"), chunk(5, b"f").finish()],
                || failed(OutOfRange, RESUME_OFFSET, Left::Persisted(3)),
            ),
        ),
        on(
            PLAIN,
            both(
                "later-overlap",
                |u| vec![resume(u, 0, b"abc"), chunk(1, b"bcdef").finish()],
                || ok(vec![Reply::Resource(6)], Left::Finalized),
            ),
        ),
        on(
            PLAIN,
            both(
                "bad-crc",
                |u| vec![resume(u, 0, b"abc").finish().bad_crc()],
                || failed(InvalidArgument, crc_mismatch(b"abc"), Left::Persisted(0)),
            ),
        ),
        on(
            PLAIN,
            both(
                "too-large",
                |u| vec![resume(u, 0, &[0; 17]).finish()],
                || failed(ResourceExhausted, TOO_LARGE, Left::Persisted(0)),
            ),
        ),
        on(
            Session {
                size: Some(5),
                ..PLAIN
            },
            both(
                "object-size-short",
                |u| vec![resume(u, 0, b"abc").finish()],
                || failed(OutOfRange, SIZE, Left::Persisted(0)),
            ),
        ),
        on(
            Session {
                size: Some(2),
                ..PLAIN
            },
            both(
                "object-size-over",
                |u| vec![resume(u, 0, b"abc")],
                || failed(OutOfRange, SIZE, Left::Persisted(0)),
            ),
        ),
        on(
            PLAIN,
            both(
                "md5-length",
                |u| vec![resume(u, 0, b"abc").finish().md5(3)],
                || failed(InvalidArgument, MD5_LENGTH, Left::Persisted(0)),
            ),
        ),
        on(
            Session {
                crc32c: Some(crc32c::crc32c(b"abc")),
                ..PLAIN
            },
            both(
                "conflicting-checksums",
                |u| vec![resume(u, 0, b"abc").finish().crc(1)],
                || failed(InvalidArgument, CONFLICTING, Left::Persisted(0)),
            ),
        ),
        // A complete object that fails its checksum ends the session.
        on(
            Session {
                crc32c: Some(1),
                ..PLAIN
            },
            both(
                "final-checksum-mismatch",
                |u| vec![resume(u, 0, b"abc").finish()],
                || failed(InvalidArgument, STORED_MISMATCH, Left::SessionGone),
            ),
        ),
        // As for single-shot writes, BidiWriteObject does not read past finish_write.
        on(
            PLAIN,
            split(
                "after-finish",
                |u| vec![resume(u, 0, b"abc").finish(), chunk(3, b"d")],
                failed(InvalidArgument, AFTER_FINISH, Left::Persisted(0)),
                ok(vec![Reply::Resource(3)], Left::Finalized),
            ),
        ),
        on(
            PLAIN,
            both(
                "unknown-session",
                |u| vec![resume(&unknown(u), 0, b"abc").finish()],
                || failed(NotFound, NO_SESSION, Left::Persisted(0)),
            ),
        ),
        on(
            PLAIN,
            both(
                "malformed-upload-id",
                |_| vec![resume("grpc-io/not-hex", 0, b"abc").finish()],
                || failed(NotFound, UNKNOWN_UPLOAD_ID, Left::Persisted(0)),
            ),
        ),
        on(
            PLAIN,
            both(
                "key-on-keyless-session",
                |u| vec![resume(u, 0, b"abc").finish().keyed()],
                || failed(InvalidArgument, key_after_keyless(), Left::Persisted(0)),
            ),
        ),
        // A finalized upload answers a stream's first message with its resource.
        on(
            Session {
                prefix: b"abc",
                finalized: true,
                ..PLAIN
            },
            both(
                "replay",
                |u| vec![resume(u, 0, b"xyz").finish()],
                || ok(vec![Reply::Resource(3)], Left::Finalized),
            ),
        ),
    ];
    run_on_sessions(&storage, cases).await;
    storage.state.catalog.close().await.unwrap();
}

/// When one resumable message breaks several rules, the first rule checked decides the answer.
#[tokio::test]
async fn grpc_resumable_write_error_precedence() {
    use Code::{FailedPrecondition, InvalidArgument, NotFound, OutOfRange, Unimplemented};
    let (_dir, storage) = setup(false).await;
    let cases = vec![
        on(
            PLAIN,
            both(
                "later-upload-id-over-checksums",
                |u| {
                    vec![
                        resume(u, 0, b"abc"),
                        chunk(3, b"d")
                            .crc(1)
                            .first(FirstMessage::UploadId(u.into())),
                    ]
                },
                || failed(InvalidArgument, LATER_UPLOAD_ID, Left::Persisted(3)),
            ),
        ),
        on(
            PLAIN,
            both(
                "checksums-over-session",
                |u| vec![resume(&unknown(u), 0, b"abc").crc(1)],
                || {
                    failed(
                        Unimplemented,
                        unsupported(RESUMABLE_CHECKSUMS),
                        Left::Persisted(0),
                    )
                },
            ),
        ),
        on(
            PLAIN,
            both(
                "session-over-crc",
                |u| vec![resume(&unknown(u), 0, b"abc").finish().bad_crc()],
                || failed(NotFound, NO_SESSION, Left::Persisted(0)),
            ),
        ),
        on(
            Session {
                prefix: b"abc",
                finalized: true,
                ..PLAIN
            },
            both(
                "replay-over-message",
                |u| vec![resume(u, 7, b"xyz").finish().bad_crc().md5(3)],
                || ok(vec![Reply::Resource(3)], Left::Finalized),
            ),
        ),
        // A replay answers with the finalized resource, so WriteObject does not report the
        // message that follows its finish_write.
        on(
            Session {
                prefix: b"abc",
                finalized: true,
                ..PLAIN
            },
            both(
                "replay-over-later-message",
                |u| vec![resume(u, 0, b"xyz").finish(), chunk(3, b"d")],
                || ok(vec![Reply::Resource(3)], Left::Finalized),
            ),
        ),
        on(
            PLAIN,
            both(
                "key-over-offset",
                |u| vec![resume(u, 5, b"abc").finish().keyed()],
                || failed(InvalidArgument, key_after_keyless(), Left::Persisted(0)),
            ),
        ),
        on(
            PREEMPTED,
            both(
                "key-over-conditions",
                |u| vec![resume(u, 0, b"abc").finish().keyed()],
                || failed(InvalidArgument, key_after_keyless(), Left::Persisted(0)),
            ),
        ),
        on(
            PREEMPTED,
            both(
                "conditions-over-checksums",
                |u| vec![resume(u, 0, b"abc").finish().md5(3)],
                || failed(FailedPrecondition, PRECONDITION, Left::Persisted(0)),
            ),
        ),
        on(
            PLAIN,
            both(
                "md5-length-over-offset",
                |u| vec![resume(u, 5, b"abc").finish().md5(3)],
                || failed(InvalidArgument, MD5_LENGTH, Left::Persisted(0)),
            ),
        ),
        on(
            PLAIN,
            both(
                "negative-offset-over-crc",
                |u| vec![resume(u, -1, b"abc").finish().bad_crc()],
                || failed(OutOfRange, RESUME_NEGATIVE, Left::Persisted(0)),
            ),
        ),
        on(
            PREFIXED,
            both(
                "resume-offset-over-crc",
                |u| vec![resume(u, 1, b"bc").finish().bad_crc()],
                || failed(OutOfRange, RESUME_OFFSET, Left::Persisted(3)),
            ),
        ),
        on(
            PLAIN,
            both(
                "crc-over-max",
                |u| vec![resume(u, 0, &[0; 17]).finish().bad_crc()],
                || failed(InvalidArgument, crc_mismatch(&[0; 17]), Left::Persisted(0)),
            ),
        ),
        // The finishing message's own checks come before WriteObject's check for a later one.
        on(
            PLAIN,
            both(
                "crc-over-later-message",
                |u| vec![resume(u, 0, b"abc").finish().bad_crc(), chunk(3, b"d")],
                || failed(InvalidArgument, crc_mismatch(b"abc"), Left::Persisted(0)),
            ),
        ),
        on(
            PREFIXED,
            both(
                "offset-over-later-message",
                |u| vec![resume(u, 0, b"abc").finish(), chunk(3, b"d")],
                || failed(OutOfRange, RESUME_OFFSET, Left::Persisted(3)),
            ),
        ),
    ];
    run_on_sessions(&storage, cases).await;
    storage.state.catalog.close().await.unwrap();
}

/// A WriteObject call that sends `messages` as `token`, with `routing` as its routing metadata,
/// and keeps its request stream open until the returned hold releases it.
fn held_open_write_object(
    storage: &Storage,
    token: &str,
    routing: Option<&str>,
    messages: &[pb::BidiWriteObjectRequest],
) -> (
    impl Future<Output = hyper::Response<tonic::body::Body>> + use<>,
    Hold,
) {
    use prost::Message as _;
    let frames = messages.iter().map(|message| {
        let encoded = write_object_request(message).encode_to_vec();
        let mut frame = vec![0];
        frame.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(&encoded);
        Ok::<_, std::convert::Infallible>(hyper::body::Frame::data(Bytes::from(frame)))
    });
    let (body, hold) = HeldOpen::new(frames);
    let mut request = hyper::Request::builder()
        .method("POST")
        .uri("/google.storage.v2.Storage/WriteObject")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {token}"));
    if let Some(routing) = routing {
        request = request.header("x-goog-request-params", routing);
    }
    let request = request
        .body(tonic::body::Body::new(http_body_util::StreamBody::new(
            body,
        )))
        .unwrap();
    let service = crate::grpc::v2::service(storage.state.clone(), MAX, storage.compat_profile);
    let response = async move { crate::grpc::v2::dispatch(service, request).await.unwrap() };
    (response, hold)
}

/// The status a WriteObject response ends with.
async fn final_status(response: hyper::Response<tonic::body::Body>) -> Status {
    use http_body_util::BodyExt;
    if let Some(status) = Status::from_header_map(response.headers()) {
        return status;
    }
    let body = response.into_body().collect().await.unwrap();
    body.trailers()
        .and_then(Status::from_header_map)
        .expect("terminal grpc-status")
}

/// A resumed WriteObject stream refused before the session lookup: who sends it, its routing
/// metadata, its messages and the status it gets.
struct EarlyRefusal {
    label: &'static str,
    token: &'static str,
    routing: Option<&'static str>,
    messages: Messages,
    code: Code,
    message: &'static str,
}

/// WriteObject reads past a resumed stream's finish_write only after the checks that need no
/// upload session, so a refusal by one of them is answered at once while the client keeps its
/// stream open: conflicting routing metadata, a malformed upload_id, a caller without
/// storage.objects.create under the strict profile, and an upload_id on a later message. A
/// refusal that needs the session is answered only once the stream ends
/// (`grpc_io_write_object_answers_a_finishing_message_after_the_stream_end`).
#[tokio::test]
async fn grpc_io_write_object_refuses_before_the_session_lookup_at_once() {
    use Code::{InvalidArgument, NotFound, PermissionDenied};
    let (_dir, storage) = setup(true).await;
    let cases = [
        EarlyRefusal {
            label: "routing",
            token: "admin",
            routing: Some("bucket=projects%2F_%2Fbuckets%2Fanother-bucket"),
            messages: |u| vec![resume(u, 0, b"abc").finish()],
            code: InvalidArgument,
            message: "Routing metadata conflicts with the request; use the same resource in x-goog-request-params and the request",
        },
        EarlyRefusal {
            label: "malformed-upload-id",
            token: "admin",
            routing: None,
            messages: |_| vec![resume("grpc-io/not-hex", 0, b"abc").finish()],
            code: NotFound,
            message: UNKNOWN_UPLOAD_ID,
        },
        EarlyRefusal {
            label: "permission",
            token: "reader",
            routing: None,
            messages: |u| vec![resume(u, 0, b"abc").finish()],
            code: PermissionDenied,
            message: "reader@example.invalid does not have storage.objects.create access to the Google Cloud Storage object. Permission 'storage.objects.create' denied on resource (or it may not exist).",
        },
        EarlyRefusal {
            label: "later-upload-id",
            token: "admin",
            routing: None,
            messages: |u| {
                vec![
                    resume(u, 0, b"abc"),
                    chunk(3, b"d")
                        .finish()
                        .first(FirstMessage::UploadId(u.into())),
                ]
            },
            code: InvalidArgument,
            message: LATER_UPLOAD_ID,
        },
    ];
    for EarlyRefusal {
        label,
        token,
        routing,
        messages,
        code,
        message,
    } in cases
    {
        let upload = open_session(&storage, label, PLAIN).await;
        let (response, Hold { waited, release }) =
            held_open_write_object(&storage, token, routing, &messages(&upload));
        tokio::pin!(response);
        let response = tokio::select! {
            response = response.as_mut() => response,
            Ok(()) = waited => panic!("{label} waited for the stream to end"),
        };
        let status = final_status(response).await;
        assert_eq!(
            (status.code(), status.message()),
            (code, message),
            "{label}"
        );
        drop(release);
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_appendable_write_message_errors() {
    use Code::{InvalidArgument, OutOfRange, ResourceExhausted, Unimplemented};
    let (_dir, storage) = setup(false).await;
    let cases = vec![
        bidi(
            "open-only",
            |n| vec![appendable(n, b"")],
            ok(
                vec![Reply::Resource(0), Reply::Persisted(0)],
                Left::Object(0),
            ),
        ),
        bidi(
            "flush-and-lookup",
            |n| {
                vec![
                    appendable(n, b"abc"),
                    chunk(3, b"d").lookup(),
                    chunk(4, b"e").flush(),
                    chunk(5, b"f"),
                ]
            },
            ok(
                vec![
                    Reply::Resource(0),
                    Reply::Persisted(0),
                    Reply::Persisted(5),
                    Reply::Persisted(6),
                ],
                Left::Object(6),
            ),
        ),
        bidi(
            "finish",
            |n| vec![appendable(n, b"abc"), chunk(3, b"d").finish()],
            ok(
                vec![Reply::Resource(0), Reply::Resource(4)],
                Left::Object(4),
            ),
        ),
        bidi(
            "finish-first",
            |n| vec![appendable(n, b"abc").finish()],
            ok(vec![Reply::Resource(3)], Left::Object(3)),
        ),
        bidi(
            "after-finish",
            |n| vec![appendable(n, b"abc").finish(), chunk(3, b"d")],
            ok(vec![Reply::Resource(3)], Left::Object(3)),
        ),
        bidi(
            "key",
            |n| vec![appendable(n, b"abc").keyed()],
            failed(Unimplemented, APPEND_KEY, Left::Nothing),
        ),
        bidi(
            "later-key",
            |n| vec![appendable(n, b"abc"), chunk(3, b"d").keyed()],
            failed(Unimplemented, APPEND_KEY, Left::Object(0)).after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "later-spec",
            |n| vec![appendable(n, b"abc"), appendable(n, b"d")],
            failed(InvalidArgument, LATER_APPEND_SPEC, Left::Object(0))
                .after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "bad-crc",
            |n| vec![appendable(n, b"abc"), chunk(3, b"d").bad_crc()],
            failed(InvalidArgument, crc_mismatch(b"d"), Left::Object(0))
                .after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "checksums-before-finish",
            |n| vec![appendable(n, b"abc"), chunk(3, b"d").crc(1)],
            failed(InvalidArgument, APPEND_CHECKSUMS, Left::Object(0))
                .after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "md5-length",
            |n| vec![appendable(n, b"abc"), chunk(3, b"").finish().md5(3)],
            failed(InvalidArgument, MD5_LENGTH, Left::Object(0)).after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "md5",
            |n| vec![appendable(n, b"abc"), chunk(3, b"").finish().md5(16)],
            failed(InvalidArgument, APPEND_MD5, Left::Object(0)).after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "negative-offset",
            |n| vec![appendable(n, b"abc"), chunk(-1, b"d")],
            failed(OutOfRange, APPEND_NEGATIVE, Left::Object(0)).after(vec![Reply::Resource(0)]),
        ),
        // A message without data has no offset to check.
        bidi(
            "negative-offset-without-data",
            |n| vec![appendable(n, b"abc"), chunk(-1, b"").flush()],
            ok(
                vec![Reply::Resource(0), Reply::Persisted(3), Reply::Persisted(3)],
                Left::Object(3),
            ),
        ),
        bidi(
            "gap",
            |n| vec![appendable(n, b"abc"), chunk(5, b"d")],
            failed(
                OutOfRange,
                "write_offset leaves a gap after the 3 bytes received; resume at persisted_size",
                Left::Object(0),
            )
            .after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "overlap",
            |n| vec![appendable(n, b"abc"), chunk(1, b"bcd").finish()],
            ok(
                vec![Reply::Resource(0), Reply::Resource(4)],
                Left::Object(4),
            ),
        ),
        bidi(
            "finish-checksum-mismatch",
            |n| vec![appendable(n, b"abc"), chunk(3, b"").finish().crc(1)],
            failed(InvalidArgument, STORED_MISMATCH, Left::Object(3))
                .after(vec![Reply::Resource(0)]),
        ),
        // A creating stream's object starts empty, so its opening offset and the size limit are
        // checked before the object is created, and a refused opening creates nothing.
        bidi(
            "too-large",
            |n| vec![appendable(n, &[0; 17])],
            failed(
                ResourceExhausted,
                "Object exceeds the upload size limit",
                Left::Nothing,
            ),
        ),
        bidi(
            "opening-gap",
            |n| vec![appendable(n, b"abc").at(2)],
            failed(OutOfRange, OPENING_GAP, Left::Nothing),
        ),
        bidi(
            "opening-checksums-before-finish",
            |n| vec![appendable(n, b"abc").crc(1)],
            failed(InvalidArgument, APPEND_CHECKSUMS, Left::Nothing),
        ),
        bidi(
            "object-size",
            |n| {
                let mut message = appendable(n, b"abc");
                if let Some(FirstMessage::WriteObjectSpec(spec)) = message.first_message.as_mut() {
                    spec.object_size = Some(3);
                }
                vec![message]
            },
            failed(
                Unimplemented,
                unsupported("appendable object_size; omit it and finalize with finish_write"),
                Left::Nothing,
            ),
        ),
        bidi(
            "resource-checksums",
            |n| {
                let mut message = appendable(n, b"abc");
                if let Some(FirstMessage::WriteObjectSpec(spec)) = message.first_message.as_mut() {
                    spec.resource.as_mut().unwrap().checksums = Some(pb::ObjectChecksums {
                        crc32c: Some(1),
                        md5_hash: Vec::new(),
                    });
                }
                vec![message]
            },
            failed(
                InvalidArgument,
                "Appendable objects accept checksums only with finish_write; omit resource.checksums and send object_checksums with finish_write=true",
                Left::Nothing,
            ),
        ),
        Case {
            label: "write-object-refuses-appendable",
            messages: |n| vec![appendable(n, b"abc").finish()],
            write_object: Some(failed(
                Unimplemented,
                unsupported("appendable/predefined_acl; omit these fields"),
                Left::Nothing,
            )),
            bidi: ok(vec![Reply::Resource(3)], Left::Object(3)),
        },
    ];
    run_on_objects(&storage, cases).await;
    storage.state.catalog.close().await.unwrap();
}

/// When one appendable message breaks several rules, the first rule checked decides the answer.
#[tokio::test]
async fn grpc_appendable_write_error_precedence() {
    use Code::{InvalidArgument, OutOfRange, Unimplemented};
    let (_dir, storage) = setup(false).await;
    let cases = vec![
        bidi(
            "key-over-later-spec",
            |n| vec![appendable(n, b"abc"), appendable(n, b"d").keyed()],
            failed(Unimplemented, APPEND_KEY, Left::Object(0)).after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "later-spec-over-crc",
            |n| vec![appendable(n, b"abc"), appendable(n, b"d").bad_crc()],
            failed(InvalidArgument, LATER_APPEND_SPEC, Left::Object(0))
                .after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "crc-over-checksums",
            |n| vec![appendable(n, b"abc"), chunk(3, b"d").crc(1).bad_crc()],
            failed(InvalidArgument, crc_mismatch(b"d"), Left::Object(0))
                .after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "checksums-over-md5",
            |n| vec![appendable(n, b"abc"), chunk(3, b"d").md5(16)],
            failed(InvalidArgument, APPEND_CHECKSUMS, Left::Object(0))
                .after(vec![Reply::Resource(0)]),
        ),
        bidi(
            "md5-over-offset",
            |n| vec![appendable(n, b"abc"), chunk(-1, b"d").finish().md5(16)],
            failed(InvalidArgument, APPEND_MD5, Left::Object(0)).after(vec![Reply::Resource(0)]),
        ),
        // The spec is checked first, then the opening message, and both before the object is
        // created.
        bidi(
            "spec-over-opening-message",
            |n| {
                let mut message = appendable(n, b"abc").bad_crc();
                if let Some(FirstMessage::WriteObjectSpec(spec)) = message.first_message.as_mut() {
                    spec.object_size = Some(3);
                }
                vec![message]
            },
            failed(
                Unimplemented,
                unsupported("appendable object_size; omit it and finalize with finish_write"),
                Left::Nothing,
            ),
        ),
        bidi(
            "opening-message-over-gap",
            |n| vec![appendable(n, b"abc").at(2).bad_crc()],
            failed(InvalidArgument, crc_mismatch(b"abc"), Left::Nothing),
        ),
        bidi(
            "opening-gap-over-max",
            |n| vec![appendable(n, &[0; 17]).at(2)],
            failed(OutOfRange, OPENING_GAP, Left::Nothing),
        ),
        // The bucket is found only when the object is created, after the opening message.
        bidi(
            "opening-gap-over-bucket",
            |n| {
                let mut message = appendable(n, b"abc").at(2);
                if let Some(FirstMessage::WriteObjectSpec(spec)) = message.first_message.as_mut() {
                    spec.resource.as_mut().unwrap().bucket = "projects/_/buckets/missing".into();
                }
                vec![message]
            },
            failed(OutOfRange, OPENING_GAP, Left::Nothing),
        ),
    ];
    run_on_objects(&storage, cases).await;
    storage.state.catalog.close().await.unwrap();
}

/// The busy-session status a message for a leased upload session gets when it cannot wait.
const BUSY: &str = "Object storage is unavailable: upload session is busy; wait for the current request to finish before sending another request for this session, or increase catalog_wait_timeout_ms (for example, 60000)";

/// Applies the session settings a server applies at startup, with `wait_millis` as
/// catalog_wait_timeout_ms. A catalog no server configured has no wait budget.
fn as_server(storage: &Storage, wait_millis: u64) {
    let config = crate::config::Config {
        catalog_wait_timeout: crate::time_limit::TimeLimit::from_millis(wait_millis),
        ..Default::default()
    };
    storage.state.catalog.apply_session_limits(&config);
}

/// A server lets a message for a session another stream holds wait for the lease, up to
/// catalog_wait_timeout_ms, and then continue at the persisted offset. QueryWriteStatus takes no
/// lease: it answers meanwhile with the persisted size and the checksums of the persisted bytes.
#[tokio::test]
async fn grpc_resumable_write_waits_for_a_leased_session() {
    let (_dir, storage) = setup(false).await;
    as_server(&storage, 60_000);
    for (rpc, upload) in leasable_uploads(&storage, "waiting").await {
        let held = lease(&storage, &upload).await;
        let messages = [resume(&upload, 3, b"def").finish()];
        let write = call(&storage, rpc, &messages);
        tokio::pin!(write);
        let early = tokio::time::timeout(std::time::Duration::from_millis(200), write.as_mut());
        assert!(early.await.is_err(), "{rpc:?} answered while leased");
        let queried = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            query_status(&storage, &upload),
        )
        .await
        .expect("QueryWriteStatus waited for the lease")
        .unwrap();
        assert_eq!(
            (queried.write_status, queried.persisted_data_checksums),
            (
                Some(pb::query_write_status_response::WriteStatus::PersistedSize(
                    3
                )),
                persisted_checksums(b"abc")
            ),
            "{rpc:?}"
        );
        drop(held);
        let (status, replies) = write.await;
        assert_eq!(
            answer(&status, replies, session_left(&storage, &upload).await),
            ok(vec![Reply::Resource(6)], Left::Finalized),
            "{rpc:?}"
        );
    }
    storage.state.catalog.close().await.unwrap();
}

/// Past catalog_wait_timeout_ms a message for a session another stream holds fails with
/// UNAVAILABLE, which the proto lists among the errors a client resumes after, and leaves the
/// session as it was; once the lease is free the same message continues the upload.
#[tokio::test]
async fn grpc_resumable_write_to_a_session_leased_past_the_catalog_wait_is_unavailable() {
    let (_dir, storage) = setup(false).await;
    let uploads = leasable_uploads(&storage, "busy").await;
    // Applied after the sessions are set up, which the short budget would also bound.
    as_server(&storage, 100);
    refuse_while_leased(&storage, uploads).await;
    storage.state.catalog.close().await.unwrap();
}

/// A catalog no server configured, as an embedded one, has no wait budget: a message for a
/// session another stream holds fails at once and leaves the session as it was. A server waits
/// first, as the two tests above pin.
#[tokio::test]
async fn grpc_resumable_write_to_a_leased_session_fails_without_waiting() {
    let (_dir, storage) = setup(false).await;
    let uploads = leasable_uploads(&storage, "leased").await;
    refuse_while_leased(&storage, uploads).await;
    storage.state.catalog.close().await.unwrap();
}

/// A session per RPC, with `abc` persisted, for the lease tests.
async fn leasable_uploads(storage: &Storage, prefix: &str) -> Vec<(Rpc, String)> {
    let mut uploads = Vec::new();
    for rpc in [Rpc::WriteObject, Rpc::Bidi] {
        let upload = open_session(storage, &format!("{prefix}-{rpc:?}"), PREFIXED).await;
        uploads.push((rpc, upload));
    }
    uploads
}

/// Takes the session lease a stream holds while it writes a message.
async fn lease(storage: &Storage, upload: &str) -> crate::storage::SessionLease {
    let id = upload.parse::<UploadId>().unwrap();
    storage
        .state
        .catalog
        .lock_resumable_session(id.session(), id.bucket())
        .await
        .unwrap()
}

/// Sends each upload's finishing message over its RPC while another holder keeps the session
/// lease past any wait, which fails with UNAVAILABLE and leaves the session as it was, and again
/// once the lease is free, which finishes the upload.
async fn refuse_while_leased(storage: &Storage, uploads: Vec<(Rpc, String)>) {
    for (rpc, upload) in uploads {
        let held = lease(storage, &upload).await;
        let messages = [resume(&upload, 3, b"def").finish()];
        let (status, replies) = call(storage, rpc, &messages).await;
        assert_eq!(
            answer(&status, replies, session_left(storage, &upload).await),
            failed(Code::Unavailable, BUSY, Left::Persisted(3)),
            "{rpc:?}"
        );
        drop(held);
        let (status, replies) = call(storage, rpc, &messages).await;
        assert_eq!(
            answer(&status, replies, session_left(storage, &upload).await),
            ok(vec![Reply::Resource(6)], Left::Finalized),
            "{rpc:?}"
        );
    }
}

/// Streams that take turns on one upload each continue at the session's persisted offset. A
/// later message of a stream may resend bytes another stream already persisted; they are
/// skipped without comparison, so the bytes persisted first stay.
#[tokio::test]
async fn grpc_resumable_streams_sharing_an_upload_keep_the_bytes_persisted_first() {
    let (_dir, storage) = setup(false).await;
    let upload = open_session(&storage, "shared", PLAIN).await;
    let (requests, incoming) = mpsc::channel(1);
    let (responses, mut replies) = mpsc::channel::<Result<pb::BidiWriteObjectResponse, Status>>(4);
    requests
        .send(Ok(resume(&upload, 0, b"abc").flush()))
        .await
        .unwrap();
    let other = async {
        let mut seen = vec![replies.recv().await.unwrap().unwrap()];
        let (status, _) = call(&storage, Rpc::WriteObject, &[resume(&upload, 3, b"XYZ")]).await;
        assert_eq!(status.code(), Code::Ok, "{status}");
        requests.send(Ok(chunk(3, b"def").flush())).await.unwrap();
        seen.push(replies.recv().await.unwrap().unwrap());
        requests.send(Ok(chunk(6, b"g").finish())).await.unwrap();
        drop(requests);
        seen
    };
    let mut stream = ReceiverStream::new(incoming);
    let (result, seen) = tokio::join!(
        storage.receive_write(&mut stream, None, WriteRpc::Bidi(&responses)),
        other
    );
    let seen: Vec<_> = seen
        .into_iter()
        .filter_map(|response: pb::BidiWriteObjectResponse| response.write_status)
        .map(reply)
        .collect();
    assert_eq!(seen, [Reply::Persisted(3), Reply::Persisted(6)]);
    let Some(pb::bidi_write_object_response::WriteStatus::Resource(object)) =
        result.unwrap().write_status
    else {
        panic!("not finalized")
    };
    assert_eq!(object.size, 7);
    assert_eq!(
        object.checksums.unwrap().crc32c,
        Some(crc32c::crc32c(b"abcXYZg"))
    );
    storage.state.catalog.close().await.unwrap();
}

/// The CRC32C and MD5 of `persisted`, as `persisted_data_checksums` reports them.
fn persisted_checksums(persisted: &[u8]) -> Option<pb::ObjectChecksums> {
    let mut hasher = crate::hashing::Hasher::default();
    hasher.update(persisted);
    let hashes = hasher.finish();
    Some(pb::ObjectChecksums {
        crc32c: Some(hashes.crc32c),
        md5_hash: hashes.md5.to_vec(),
    })
}

/// The proto's WriteObjectResponse, BidiWriteObjectResponse and QueryWriteStatusResponse each
/// say: "If persisted_size is set, contains checksums of persisted data." A resource carries
/// its own checksums instead.
#[tokio::test]
async fn grpc_resumable_persisted_sizes_carry_the_persisted_checksums() {
    use pb::query_write_status_response::WriteStatus as Queried;
    let (_dir, storage) = setup(false).await;
    let upload = open_session(&storage, "checksummed", PLAIN).await;
    let query = async || query_status(&storage, &upload).await.unwrap();
    let fresh = query().await;
    assert_eq!(fresh.write_status, Some(Queried::PersistedSize(0)));
    assert_eq!(fresh.persisted_data_checksums, persisted_checksums(b""));

    let send = async |request| {
        crate::grpc::v2::dispatch(
            crate::grpc::v2::service(storage.state.clone(), MAX, storage.compat_profile),
            request,
        )
        .await
        .unwrap()
    };
    let requests = [write_object_request(&resume(&upload, 0, b"abc"))];
    let (status, responses): (_, Vec<pb::WriteObjectResponse>) = rpc_status(
        "/google.storage.v2.Storage/WriteObject",
        None,
        &requests,
        send,
    )
    .await;
    assert_eq!(status.code(), Code::Ok, "{status}");
    let [written] = responses.as_slice() else {
        panic!("{responses:?}")
    };
    assert_eq!(
        written.write_status,
        Some(pb::write_object_response::WriteStatus::PersistedSize(3))
    );
    assert_eq!(
        written.persisted_data_checksums,
        persisted_checksums(b"abc")
    );

    let messages = [chunk(3, b"def")
        .flush()
        .first(FirstMessage::UploadId(upload.clone()))];
    let (status, responses): (_, Vec<pb::BidiWriteObjectResponse>) = rpc_status(
        "/google.storage.v2.Storage/BidiWriteObject",
        None,
        &messages,
        send,
    )
    .await;
    assert_eq!(status.code(), Code::Ok, "{status}");
    // The flush is answered, and so is the close that follows it.
    let [flushed, closed] = responses.as_slice() else {
        panic!("{responses:?}")
    };
    assert_eq!(
        flushed.write_status,
        Some(pb::bidi_write_object_response::WriteStatus::PersistedSize(
            6
        ))
    );
    assert_eq!(
        flushed.persisted_data_checksums,
        persisted_checksums(b"abcdef")
    );
    assert_eq!(closed, flushed);
    let queried = query().await;
    assert_eq!(queried.write_status, Some(Queried::PersistedSize(6)));
    assert_eq!(
        queried.persisted_data_checksums,
        persisted_checksums(b"abcdef")
    );

    let (status, replies) = call(&storage, Rpc::Bidi, &[resume(&upload, 6, b"").finish()]).await;
    assert_eq!(
        (status.code(), replies),
        (Code::Ok, vec![Reply::Resource(6)])
    );
    let finished = query().await;
    assert!(matches!(finished.write_status, Some(Queried::Resource(_))));
    assert_eq!(finished.persisted_data_checksums, None);
    storage.state.catalog.close().await.unwrap();
}

/// A spec naming a predefined ACL, an appendable resumable write or a resource field the
/// emulator does not model is refused with UNIMPLEMENTED and ErrorInfo on all three RPCs that
/// take a spec, before anything is created.
#[tokio::test]
async fn grpc_write_specs_refuse_unhandled_fields() {
    type SpecEdit = fn(&mut pb::WriteObjectSpec);
    let (_dir, storage) = setup(false).await;
    let send = async |request| {
        crate::grpc::v2::dispatch(
            crate::grpc::v2::service(storage.state.clone(), MAX, storage.compat_profile),
            request,
        )
        .await
        .unwrap()
    };
    let acl_or_appendable = unsupported("appendable/predefined_acl; omit these fields");
    let fields = unsupported(
        "write_object_spec.resource fields; use content_type, fixed/custom metadata and holds",
    );
    let edits: [(&str, SpecEdit, &str); 4] = [
        (
            "predefined-acl",
            |spec| spec.predefined_acl = "publicRead".into(),
            &acl_or_appendable,
        ),
        (
            "acl",
            |spec| {
                spec.resource.as_mut().unwrap().acl = vec![pb::ObjectAccessControl {
                    role: "READER".into(),
                    entity: "allUsers".into(),
                    ..Default::default()
                }];
            },
            &fields,
        ),
        (
            "kms-key",
            |spec| spec.resource.as_mut().unwrap().kms_key = "key".into(),
            &fields,
        ),
        (
            "retention",
            |spec| {
                spec.resource.as_mut().unwrap().retention = Some(Default::default());
            },
            &fields,
        ),
    ];
    for (label, edit, message) in edits {
        let mut refused = spec(label);
        edit(&mut refused);
        let single = [chunk(0, b"abc")
            .finish()
            .first(FirstMessage::WriteObjectSpec(refused.clone()))];
        let (written, _) = call(&storage, Rpc::WriteObject, &single).await;
        let (bidi, _) = call(&storage, Rpc::Bidi, &single).await;
        let start = [pb::StartResumableWriteRequest {
            write_object_spec: Some(refused),
            ..Default::default()
        }];
        let (started, _): (_, Vec<pb::StartResumableWriteResponse>) = rpc_status(
            "/google.storage.v2.Storage/StartResumableWrite",
            None,
            &start,
            send,
        )
        .await;
        for status in [written, bidi, started] {
            assert_eq!(
                (status.code(), status.message()),
                (Code::Unimplemented, message),
                "{label}"
            );
            assert_bad_request(&status, label);
        }
        assert_eq!(object_left(&storage, label).await, Left::Nothing, "{label}");
    }
    // StartResumableWrite refuses an appendable spec; BidiWriteObject routes it to Rapid append.
    let mut appendable = spec("appendable");
    appendable.appendable = Some(true);
    let start = [pb::StartResumableWriteRequest {
        write_object_spec: Some(appendable),
        ..Default::default()
    }];
    let (status, _): (_, Vec<pb::StartResumableWriteResponse>) = rpc_status(
        "/google.storage.v2.Storage/StartResumableWrite",
        None,
        &start,
        send,
    )
    .await;
    assert_eq!(
        (status.code(), status.message()),
        (Code::Unimplemented, acl_or_appendable.as_str())
    );
    assert_bad_request(&status, "appendable");
    storage.state.catalog.close().await.unwrap();
}

/// Asserts that `status` carries one ErrorInfo with the reason and domain of the emulator's
/// UNIMPLEMENTED refusals.
#[track_caller]
fn assert_bad_request(status: &Status, label: &str) {
    use crate::grpc::generated::google::rpc::{ErrorInfo, Status as RpcStatus};
    use prost::Message;
    let details = RpcStatus::decode(status.details()).unwrap();
    let [detail] = details.details.as_slice() else {
        panic!("{label}: {:?}", details.details)
    };
    assert_eq!(
        detail.type_url, "type.googleapis.com/google.rpc.ErrorInfo",
        "{label}"
    );
    let info = ErrorInfo::decode(detail.value.as_slice()).unwrap();
    assert_eq!(
        (info.reason.as_str(), info.domain.as_str()),
        ("badRequest", "global"),
        "{label}"
    );
}
