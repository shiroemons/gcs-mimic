use bytes::Bytes;
mod memory;

#[test]
fn upload_chunks_split_message_data_at_channel_chunk_size() {
    use super::upload::{CHUNK_BYTES as UPLOAD_CHUNK_BYTES, chunks as upload_chunks};
    let data = Bytes::from(
        (0..(2 * UPLOAD_CHUNK_BYTES + 3))
            .map(|i| i as u8)
            .collect::<Vec<_>>(),
    );
    let chunks: Vec<_> = upload_chunks(&data).collect();
    assert_eq!(
        chunks.iter().map(Bytes::len).collect::<Vec<_>>(),
        [UPLOAD_CHUNK_BYTES, UPLOAD_CHUNK_BYTES, 3]
    );
    assert_eq!(chunks.concat(), data);
    let exact = data.slice(..UPLOAD_CHUNK_BYTES);
    assert_eq!(
        upload_chunks(&exact).collect::<Vec<_>>(),
        std::slice::from_ref(&exact)
    );
    assert_eq!(upload_chunks(&Bytes::new()).count(), 0);
}
