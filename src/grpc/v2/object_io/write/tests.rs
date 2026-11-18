//! WriteObject, non-appendable BidiWriteObject, StartResumableWrite and QueryWriteStatus with
//! customer-supplied keys. The request that starts a write chooses the key; the rules follow
//! Cloud Storage's answers recorded over gRPC on 2026-11-15 unless a test says otherwise.

use super::super::tests::{message, setup, write};
use super::*;
use crate::csek::key_sha256;
use crate::grpc::test_support::{assert_customer_encryption, key_params};
use crate::grpc::v2::test_support::undispatched;
use crate::object::ObjectRecord;
use crate::storage::Catalog;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use pb::bidi_write_object_request::FirstMessage;
use pb::bidi_write_object_response::WriteStatus;
use pb::query_write_status_response::WriteStatus as QueriedStatus;
use std::sync::Arc;

const FIRST: [u8; 32] = [1; 32];
const SECOND: [u8; 32] = [2; 32];

/// The size of one write granule: a resumable stream persists whole granules until
/// finish_write (recorded 2026-11-18).
const GRANULE: i64 = 256 * 1024;

/// One granule of data, the least a resumable stream persists without finish_write.
fn granule() -> Vec<u8> {
    vec![b's'; 256 * 1024]
}

/// The first message of a one-shot write of `data`, carrying `key`.
fn first(
    name: &str,
    data: &[u8],
    finish: bool,
    key: Option<&[u8; 32]>,
) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        finish_write: finish,
        common_object_request_params: key.and_then(key_params),
        ..message(name, data)
    }
}

/// A later message of a write stream, carrying `data` at `offset` and `key`.
fn later(
    offset: i64,
    data: &[u8],
    finish: bool,
    key: Option<&[u8; 32]>,
) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        first_message: None,
        write_offset: offset,
        ..first("ignored", data, finish, key)
    }
}

/// The first message of a stream that resumes `upload_id`.
fn resume(
    upload_id: &str,
    offset: i64,
    data: &[u8],
    finish: bool,
    key: Option<&[u8; 32]>,
) -> pb::BidiWriteObjectRequest {
    pb::BidiWriteObjectRequest {
        first_message: Some(FirstMessage::UploadId(upload_id.into())),
        ..later(offset, data, finish, key)
    }
}

async fn start(storage: &Storage, name: &str, key: Option<&[u8; 32]>) -> Result<String, Status> {
    storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(super::super::tests::spec(name)),
                common_object_request_params: key.and_then(key_params),
                ..Default::default()
            }),
            None,
        )
        .await
        .map(|response| response.into_inner().upload_id)
}

async fn query(storage: &Storage, upload_id: &str, key: Option<&[u8; 32]>) -> QueriedStatus {
    storage
        .query_write_status(
            undispatched(pb::QueryWriteStatusRequest {
                upload_id: upload_id.into(),
                common_object_request_params: key.and_then(key_params),
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner()
        .write_status
        .unwrap()
}

/// Opens the stored object's data with `key`, as every API's read does.
async fn read_with(
    catalog: &Catalog,
    name: &str,
    key: Option<&[u8; 32]>,
) -> Result<ObjectRecord, ObjectError> {
    catalog
        .open_object_keyed(
            live("grpc-io", name),
            Default::default(),
            PERMISSIVE,
            key.map(|key| CustomerKey::from_sha256(key_sha256(key))),
            crate::object::ReadAhead::None,
        )
        .await
        .map(|(record, _)| record)
}

async fn missing(catalog: &Catalog, name: &str) -> bool {
    read_with(catalog, name, None)
        .await
        .is_err_and(|error| error == ObjectError::ObjectNotFound)
}

fn customer_encryption(key: &[u8; 32]) -> Option<pb::CustomerEncryption> {
    Some(pb::CustomerEncryption {
        encryption_algorithm: crate::csek::ALGORITHM.into(),
        key_sha256_bytes: key_sha256(key).to_vec(),
    })
}

#[track_caller]
fn resource(status: WriteStatus) -> pb::Object {
    let WriteStatus::Resource(object) = status else {
        panic!("write not finalized: {status:?}")
    };
    object
}

/// The finished object is encrypted with `key`, and the resource shows both checksums of `data`.
#[track_caller]
fn assert_encrypted(object: &pb::Object, key: &[u8; 32], data: &[u8]) {
    assert_eq!(object.customer_encryption, customer_encryption(key));
    let checksums = object.checksums.as_ref().expect("checksums");
    assert_eq!(checksums.crc32c, Some(crc32c::crc32c(data)));
    assert_eq!(checksums.md5_hash.len(), 16);
}

/// Only `key` opens the object's data.
async fn assert_readable_only_with(catalog: &Catalog, name: &str, key: &[u8; 32]) {
    assert!(read_with(catalog, name, Some(key)).await.is_ok());
    assert_eq!(
        read_with(catalog, name, None).await.err(),
        Some(ObjectError::CustomerEncryption(
            CsekError::ResourceIsEncrypted
        ))
    );
    let other = if key == &FIRST { &SECOND } else { &FIRST };
    assert_eq!(
        read_with(catalog, name, Some(other)).await.err(),
        Some(ObjectError::CustomerEncryption(CsekError::KeyIncorrect))
    );
}

#[tokio::test]
async fn grpc_write_with_a_key_encrypts_and_a_keyless_overwrite_does_not() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    let keyed = resource(
        write(
            &storage,
            vec![first("object", b"secret", true, Some(&FIRST))],
        )
        .await
        .unwrap(),
    );
    assert_encrypted(&keyed, &FIRST, b"secret");
    assert_readable_only_with(catalog, "object", &FIRST).await;

    // A write without a key creates an unencrypted new generation.
    let plain = resource(
        write(&storage, vec![first("object", b"plain", true, None)])
            .await
            .unwrap(),
    );
    assert_eq!(plain.customer_encryption, None);
    assert_ne!(plain.generation, keyed.generation);
    assert!(read_with(catalog, "object", None).await.is_ok());
    assert_eq!(
        read_with(catalog, "object", Some(&FIRST)).await.err(),
        Some(ObjectError::CustomerEncryption(
            CsekError::ResourceNotEncrypted
        ))
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_write_later_messages_cannot_change_the_first_key() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    // Recorded: a second message with another key succeeds, encrypted with the first key.
    for (name, later_key) in [("repeated", Some(&FIRST)), ("changed", Some(&SECOND))] {
        let messages = vec![
            first(name, b"sec", false, Some(&FIRST)),
            later(3, b"ret", true, later_key),
        ];
        let object = resource(write(&storage, messages).await.unwrap());
        assert_encrypted(&object, &FIRST, b"secret");
        assert_readable_only_with(catalog, name, &FIRST).await;
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_write_key_after_a_keyless_start_or_malformed_publishes_nothing() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    // Cloud Storage answered INTERNAL here; the emulator names the misplaced key instead.
    let late = vec![
        first("late", b"sec", false, None),
        later(3, b"ret", true, Some(&FIRST)),
    ];
    assert_customer_encryption(write(&storage, late).await, CsekError::ResourceNotEncrypted);
    assert!(missing(catalog, "late").await);

    let mut malformed = first("malformed", b"secret", true, Some(&FIRST));
    if let Some(params) = malformed.common_object_request_params.as_mut() {
        params.encryption_key_bytes.truncate(16);
    }
    assert_customer_encryption(
        write(&storage, vec![malformed]).await,
        CsekError::KeyFormatInvalid,
    );
    assert!(missing(catalog, "malformed").await);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_resumable_key_binds_at_start_and_status_ignores_keys() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    let mut malformed = key_params(&FIRST).unwrap_or_default();
    malformed.encryption_algorithm = "AES128".into();
    let refused = storage
        .start_resumable_write(
            Request::new(pb::StartResumableWriteRequest {
                write_object_spec: Some(super::super::tests::spec("refused")),
                common_object_request_params: Some(malformed),
                ..Default::default()
            }),
            None,
        )
        .await;
    assert_customer_encryption(refused, CsekError::AlgorithmInvalid);

    let upload = start(&storage, "resumed", Some(&FIRST)).await.unwrap();
    // upload_id messages need no key; one with another key is ignored, as Cloud Storage does.
    write(&storage, vec![resume(&upload, 0, &granule(), false, None)])
        .await
        .unwrap();
    write(
        &storage,
        vec![resume(&upload, GRANULE, &granule(), false, Some(&SECOND))],
    )
    .await
    .unwrap();
    // QueryWriteStatus answers even with a wrong key, on an open and on a finished session.
    assert_eq!(
        query(&storage, &upload, Some(&SECOND)).await,
        QueriedStatus::PersistedSize(2 * GRANULE)
    );
    let object = resource(
        write(
            &storage,
            vec![resume(&upload, 2 * GRANULE, b"et", true, None)],
        )
        .await
        .unwrap(),
    );
    let secret = [granule(), granule(), b"et".to_vec()].concat();
    assert_encrypted(&object, &FIRST, &secret);
    assert_readable_only_with(catalog, "resumed", &FIRST).await;
    for key in [None, Some(&SECOND)] {
        let QueriedStatus::Resource(finished) = query(&storage, &upload, key).await else {
            panic!("session not finished")
        };
        assert_encrypted(&finished, &FIRST, &secret);
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_keyless_session_refuses_a_key_on_one_message_and_stays_usable() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    let upload = start(&storage, "keyless", None).await.unwrap();
    write(&storage, vec![resume(&upload, 0, &granule(), false, None)])
        .await
        .unwrap();
    // A local decision: Cloud Storage answered INTERNAL for this message.
    assert_customer_encryption(
        write(
            &storage,
            vec![resume(&upload, GRANULE, b"in", true, Some(&FIRST))],
        )
        .await,
        CsekError::ResourceNotEncrypted,
    );
    // The same refusal on a later message of a stream keeps what earlier messages persisted.
    let messages = vec![
        resume(&upload, GRANULE, &granule(), false, None),
        later(2 * GRANULE, b"n", true, Some(&FIRST)),
    ];
    assert_customer_encryption(
        write(&storage, messages).await,
        CsekError::ResourceNotEncrypted,
    );
    assert_eq!(
        query(&storage, &upload, None).await,
        QueriedStatus::PersistedSize(2 * GRANULE)
    );
    assert!(missing(catalog, "keyless").await);
    let object = resource(
        write(
            &storage,
            vec![resume(&upload, 2 * GRANULE, b"n", true, None)],
        )
        .await
        .unwrap(),
    );
    assert_eq!(object.customer_encryption, None);
    assert!(read_with(catalog, "keyless", None).await.is_ok());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_resumable_key_survives_a_restart() {
    let (dir, mut storage) = setup(false).await;
    let upload = start(&storage, "restarted", Some(&FIRST)).await.unwrap();
    write(
        &storage,
        vec![resume(&upload, 0, &granule(), false, Some(&FIRST))],
    )
    .await
    .unwrap();

    storage.state.catalog.close().await.unwrap();
    let state = Arc::get_mut(&mut storage.state).expect("no other state handle");
    state.catalog = Catalog::open(&dir.0).await.unwrap();
    let object = resource(
        write(&storage, vec![resume(&upload, GRANULE, b"ret", true, None)])
            .await
            .unwrap(),
    );
    let secret = [granule(), b"ret".to_vec()].concat();
    assert_encrypted(&object, &FIRST, &secret);
    assert_readable_only_with(&storage.state.catalog, "restarted", &FIRST).await;
    let QueriedStatus::Resource(finished) = query(&storage, &upload, None).await else {
        panic!("session not finished")
    };
    assert_encrypted(&finished, &FIRST, &secret);
    storage.state.catalog.close().await.unwrap();
}
