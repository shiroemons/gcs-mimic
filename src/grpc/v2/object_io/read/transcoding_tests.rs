//! ReadObject of an object stored with `content_encoding: gzip`. The proto and the RPC reference
//! say nothing about decompressive transcoding over Storage v2 and nothing was recorded, so
//! ReadObject returns the stored bytes as they are, a local choice (SPEC-GRPC-LOCAL-CHOICES);
//! the JSON and XML reads are BEHAVIOR-OBJECT-017.

use super::super::tests::{message, setup, write};
use super::test_support::{body, read};
use super::*;

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut encoder, data).unwrap();
    encoder.finish().unwrap()
}

#[tokio::test]
async fn read_object_returns_gzip_encoded_bytes_as_stored() {
    let (_dir, storage) = setup(false).await;
    let stored = gzip(b"text that a transcoding read would return decompressed");
    let mut upload = message("gzipped", &stored);
    if let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
        upload.first_message.as_mut()
        && let Some(resource) = spec.resource.as_mut()
    {
        resource.content_encoding = "gzip".into();
        resource.content_type = "text/plain".into();
    }
    write(&storage, vec![upload]).await.unwrap();

    let request = pb::ReadObjectRequest {
        bucket: "projects/_/buckets/grpc-io".into(),
        object: "gzipped".into(),
        ..Default::default()
    };
    let messages = read(&storage, request.clone()).await.unwrap();
    assert_eq!(body(&messages), stored);
    let metadata = messages.first().unwrap().metadata.as_ref().unwrap();
    assert_eq!(metadata.content_encoding, "gzip");
    assert_eq!(metadata.size, i64::try_from(stored.len()).unwrap());
    // A range selects stored bytes as well.
    let ranged = read(
        &storage,
        pb::ReadObjectRequest {
            read_offset: 2,
            read_limit: 3,
            ..request
        },
    )
    .await
    .unwrap();
    assert_eq!(body(&ranged), stored[2..5]);
    storage.state.catalog.close().await.unwrap();
}
