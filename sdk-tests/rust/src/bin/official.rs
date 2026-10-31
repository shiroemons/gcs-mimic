use gcs_mimic_rust_sdk_scenarios::{Result, cleanup, control, create_bucket, guarded_origin};
use google_cloud_auth::credentials::anonymous;
use google_cloud_storage::client::Storage;
use std::io;

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = guarded_origin()?;
    let control = control(&endpoint).await?;
    let storage = Storage::builder()
        .with_endpoint(endpoint.as_str().trim_end_matches('/'))
        .with_credentials(anonymous::Builder::new().build())
        .with_resumable_upload_threshold(0_usize)
        .with_resumable_upload_buffer_size(256 * 1024_usize)
        .build()
        .await?;
    let bucket = create_bucket(&control, "official").await?;
    let parent = format!("projects/_/buckets/{bucket}");
    let result: Result<()> = async {
        let payload: Vec<u8> = (0..576 * 1024).map(|index| index as u8).collect();
        let object = storage
            .write_object(
                &parent,
                "stream/日本語 +%?.bin",
                bytes::Bytes::from(payload.clone()),
            )
            .set_content_type("application/octet-stream")
            .set_if_generation_match(0)
            .set_metadata([("fixture", "official-rust")])
            .send_buffered()
            .await?;
        if object.generation <= 0 || object.size != payload.len() as i64 {
            return Err(io::Error::other("official SDK generation/size differ").into());
        }
        let mut reader = storage
            .read_object(&parent, "stream/日本語 +%?.bin")
            .send()
            .await?;
        let mut offset = 0;
        while let Some(chunk) = reader.next().await.transpose()? {
            if payload.get(offset..offset + chunk.len()) != Some(chunk.as_ref()) {
                return Err(io::Error::other("official SDK streamed bytes differ").into());
            }
            offset += chunk.len();
        }
        if offset != payload.len() {
            return Err(io::Error::other("official SDK download truncated").into());
        }
        println!("official-rust-http: bucket, resumable, generation, metadata, streamed read PASS");
        Ok(())
    }
    .await;
    let cleanup_result = cleanup(&control, &bucket, &["stream/日本語 +%?.bin"]).await;
    result.and(cleanup_result)
}
