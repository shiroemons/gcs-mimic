//! Creates a bucket in a local gcs-mimic, uploads, lists, downloads and deletes an
//! object, and removes the bucket again.

use google_cloud_auth::credentials::anonymous;
use google_cloud_storage::client::{Storage, StorageControl};
use google_cloud_storage::model::Bucket;
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const OBJECT: &str = "hello.txt";

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = env_or("GCS_MIMIC_ENDPOINT", "http://localhost:4443");
    let grpc_endpoint = env_or("GCS_MIMIC_GRPC_ENDPOINT", "http://localhost:4444");

    // `Storage` reads and writes objects over HTTP; `StorageControl` manages buckets
    // and lists objects over gRPC, which gcs-mimic serves on a separate listener.
    let storage = Storage::builder()
        .with_endpoint(endpoint)
        .with_credentials(anonymous::Builder::new().build())
        .build()
        .await?;
    let control = StorageControl::builder()
        .with_endpoint(grpc_endpoint)
        .with_credentials(anonymous::Builder::new().build())
        .build()
        .await?;

    let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let bucket_id = format!("example-rust-{nanos:x}");
    control
        .create_bucket()
        .set_parent("projects/_")
        .set_bucket_id(&bucket_id)
        .set_bucket(Bucket::new().set_project("projects/local-project"))
        .send()
        .await?;
    println!("created bucket {bucket_id}");

    let bucket = format!("projects/_/buckets/{bucket_id}");
    let result = upload_list_download(&storage, &control, &bucket).await;
    // Clean up even when a step failed, then report the first error.
    let cleanup = delete_bucket(&control, &bucket).await;
    result.and(cleanup)
}

async fn upload_list_download(
    storage: &Storage,
    control: &StorageControl,
    bucket: &str,
) -> Result<()> {
    storage
        .write_object(bucket, OBJECT, "Hello from the Rust client!\n")
        .set_content_type("text/plain")
        .send_buffered()
        .await?;
    println!("uploaded {OBJECT}");

    let listed = control.list_objects().set_parent(bucket).send().await?;
    for object in listed.objects {
        println!("listed {} ({} bytes)", object.name, object.size);
    }

    let mut reader = storage.read_object(bucket, OBJECT).send().await?;
    let mut content = Vec::new();
    while let Some(chunk) = reader.next().await.transpose()? {
        content.extend_from_slice(&chunk);
    }
    print!("downloaded: {}", String::from_utf8(content)?);
    Ok(())
}

/// Deletes every object in the bucket, then the bucket itself.
async fn delete_bucket(control: &StorageControl, bucket: &str) -> Result<()> {
    let listed = control.list_objects().set_parent(bucket).send().await?;
    for object in listed.objects {
        control
            .delete_object()
            .set_bucket(bucket)
            .set_object(&object.name)
            .send()
            .await?;
        println!("deleted {}", object.name);
    }
    control.delete_bucket().set_name(bucket).send().await?;
    println!("deleted bucket");
    Ok(())
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
