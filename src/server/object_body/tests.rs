use super::*;
use http_body_util::BodyExt;

fn activity() -> watch::Sender<ObjectActivity> {
    watch::channel(ObjectActivity::default()).0
}

/// A file holding `size` distinct bytes, and a reader over it.
fn fixture(size: usize) -> (crate::test_support::TestDir, Vec<u8>, ObjectReader) {
    let dir = crate::test_support::TestDir::new();
    let path = dir.0.join("blob");
    let contents: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
    std::fs::write(&path, &contents).unwrap();
    let file = std::fs::File::open(&path).unwrap();
    let reader = ObjectReader::bounded_for_test(file, size as u64);
    (dir, contents, reader)
}

#[tokio::test]
async fn the_body_carries_every_byte_across_the_frame_boundaries() {
    for size in [
        1,
        1024,
        64 * 1024,
        256 * 1024,
        256 * 1024 + 1,
        1024 * 1024,
        3 * 1024 * 1024 + 7,
    ] {
        let (_dir, contents, reader) = fixture(size);
        let body = ObjectBody::new(reader, activity());
        assert_eq!(body.collect().await.unwrap().to_bytes(), contents, "{size}");
    }
}

#[tokio::test]
async fn an_empty_object_ends_at_once() {
    let (_dir, _contents, reader) = fixture(0);
    let mut body = ObjectBody::new(reader, activity());
    assert!(body.is_end_stream());
    assert!(body.frame().await.is_none());
}

#[tokio::test]
async fn the_end_is_known_with_the_last_frame() {
    let (_dir, contents, reader) = fixture(1024);
    let mut body = ObjectBody::new(reader, activity());
    assert!(!body.is_end_stream());
    let frame = body.frame().await.unwrap().unwrap();
    assert_eq!(frame.into_data().unwrap(), contents);
    assert!(body.is_end_stream());
    assert!(body.frame().await.is_none());
}

/// A frame never exceeds one read buffer, which is what bounds a slow client's memory.
#[tokio::test]
async fn one_frame_stays_within_one_buffer() {
    let (_dir, _contents, reader) = fixture(3 * 1024 * 1024);
    let mut body = ObjectBody::new(reader, activity());
    let mut frames = 0;
    while let Some(frame) = body.frame().await {
        assert!(frame.unwrap().into_data().unwrap().len() <= 1024 * 1024);
        frames += 1;
    }
    assert!(frames >= 3);
}

/// A blob shorter than its record must abort the response, not end it as if it were complete.
#[tokio::test]
async fn a_short_file_fails_after_the_bytes_it_has() {
    let dir = crate::test_support::TestDir::new();
    let path = dir.0.join("blob");
    std::fs::write(&path, b"short").unwrap();
    let reader = ObjectReader::bounded_for_test(std::fs::File::open(&path).unwrap(), 10);
    let mut body = ObjectBody::new(reader, activity());
    let frame = body.frame().await.unwrap().unwrap();
    assert_eq!(frame.into_data().unwrap(), b"short"[..]);
    let error = body.frame().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert!(body.frame().await.is_none());
    assert!(body.is_end_stream());
}

/// A response body counts as activity for the limit on a silent transfer.
#[tokio::test]
async fn each_frame_records_progress() {
    let (_dir, _contents, reader) = fixture(1024);
    let activity = activity();
    let mut progress = activity.subscribe();
    progress.borrow_and_update();
    let mut body = ObjectBody::new(reader, activity.clone());
    body.frame().await.unwrap().unwrap();
    assert!(progress.has_changed().unwrap());
}

/// A delete or overwrite can unlink the blob while its download is under way.
#[tokio::test]
async fn an_unlinked_blob_still_reads_through_its_descriptor() {
    let (dir, contents, reader) = fixture(1024);
    let body = ObjectBody::new(reader, activity());
    std::fs::remove_file(dir.0.join("blob")).unwrap();
    assert_eq!(body.collect().await.unwrap().to_bytes(), contents);
}
