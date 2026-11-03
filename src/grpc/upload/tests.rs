use super::*;

fn piece(offset: u64, content: &'static [u8]) -> IntakePiece {
    IntakePiece {
        offset,
        content: Bytes::from_static(content),
        crc32c: Some(crc32c::crc32c(content)),
        finish: false,
    }
}

#[tokio::test]
async fn intake_overlap_modes_hash_only_new_bytes_and_keep_bounded_chunks() {
    let expected = ExpectedHashes {
        crc32c: Some(crc32c::crc32c(b"abcd")),
        md5: None,
    };
    let mut intake = Intake::new(expected, 4, Some(4), Resend::DiscardOverlap);
    let (tx, mut rx) = mpsc::channel(2);
    intake.accept(piece(0, b"abc"), &tx).await.unwrap();
    let mut last = piece(1, b"bcd");
    last.finish = true;
    intake.accept(last, &tx).await.unwrap();
    assert!(
        matches!(rx.recv().await.unwrap().unwrap(), UploadChunk::Data(data) if data == b"abc"[..])
    );
    assert!(
        matches!(rx.recv().await.unwrap().unwrap(), UploadChunk::Data(data) if data == b"d"[..])
    );
    intake.finish(&tx, EndMarker::WithHashes).await.unwrap();
    assert!(
        matches!(rx.recv().await.unwrap().unwrap(), UploadChunk::EndWithHashes(hashes) if hashes == expected)
    );
    let mut intake = Intake::new(Default::default(), 4, None, Resend::Reject);
    intake.accept(piece(0, b"abc"), &tx).await.unwrap();
    assert_eq!(
        intake.accept(piece(1, b"bcd"), &tx).await,
        Err(IntakeError::Offset)
    );
}

#[tokio::test]
async fn invalid_intake_never_stages_bytes_or_a_completion_marker() {
    let (tx, mut rx) = mpsc::channel(2);
    let mut bad_crc = piece(0, b"abc");
    bad_crc.crc32c = Some(0);
    let mut short = piece(0, b"a");
    short.finish = true;
    for (piece, max, total, error) in [
        (
            bad_crc,
            3,
            None,
            IntakeError::ChunkCrc {
                computed: crc32c::crc32c(b"abc"),
                client: 0,
            },
        ),
        (piece(1, b"a"), 3, None, IntakeError::Offset),
        (piece(0, b"abc"), 2, None, IntakeError::TooLarge),
        (short, 3, Some(3), IntakeError::SizeMismatch),
    ] {
        let mut intake = Intake::new(Default::default(), max, total, Resend::DiscardOverlap);
        assert_eq!(intake.accept(piece, &tx).await, Err(error));
        assert!(rx.try_recv().is_err());
    }
    let intake = Intake::new(
        ExpectedHashes {
            md5: Some([1; 16]),
            crc32c: None,
        },
        3,
        None,
        Resend::Reject,
    );
    assert_eq!(
        intake.finish(&tx, EndMarker::Plain).await,
        Err(IntakeError::ObjectChecksum)
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn stopped_consumer_wakes_a_producer_waiting_for_client_input() {
    let (tx, rx) = mpsc::channel(2);
    let mut stream = tonic::codegen::tokio_stream::pending::<()>();
    drop(rx);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), next(&mut stream, &tx))
        .await
        .unwrap();
    assert_eq!(result, Err(IntakeError::ConsumerStopped));
}

#[test]
fn upload_completion_selects_storage_roots_and_keeps_input_errors() {
    let input = || Err(Status::invalid_argument("bad chunk CRC"));
    let failure =
        completed(input(), Err(ObjectError::PreconditionFailed), Status::from).unwrap_err();
    assert_eq!(failure.code(), tonic::Code::FailedPrecondition);
    let failure = completed(
        input(),
        Err(ObjectError::body_aborted(std::io::Error::other(
            "producer stopped",
        ))),
        Status::from,
    )
    .unwrap_err();
    assert_eq!(failure.message(), "bad chunk CRC");
    let record =
        crate::grpc::test_support::object_record("test-bucket", std::time::SystemTime::now());
    assert_eq!(
        completed(input(), Ok(record), Status::from)
            .unwrap_err()
            .message(),
        "bad chunk CRC"
    );
}
