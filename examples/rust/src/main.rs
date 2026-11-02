//! Uses the official Rust client against a local gcs-mimic.

use google_cloud_auth::credentials::anonymous;
use google_cloud_storage::client::{Storage, StorageControl};
use google_cloud_storage::model::Bucket;
use std::time::{SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint =
        std::env::var("GCS_MIMIC_ENDPOINT").unwrap_or_else(|_| "http://localhost:4443".into());
    let grpc_endpoint =
        std::env::var("GCS_MIMIC_GRPC_ENDPOINT").unwrap_or_else(|_| "http://localhost:4444".into());

    // `Storage` reads and writes objects over HTTP. `StorageControl` manages buckets
    // and lists and deletes objects over gRPC, which gcs-mimic serves on its own port.
    // Anonymous credentials keep both clients away from Google sign-in.
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

    storage
        .write_object(&bucket, "hello.txt", "Hello from the Rust client!\n")
        .send_buffered()
        .await?;
    println!("uploaded hello.txt");

    let listed = control.list_objects().set_parent(&bucket).send().await?;
    for object in listed.objects {
        println!("listed {}", object.name);
    }

    let mut reader = storage.read_object(&bucket, "hello.txt").send().await?;
    let mut content = Vec::new();
    while let Some(chunk) = reader.next().await.transpose()? {
        content.extend_from_slice(&chunk);
    }
    print!("downloaded: {}", String::from_utf8(content)?);

    control
        .delete_object()
        .set_bucket(&bucket)
        .set_object("hello.txt")
        .send()
        .await?;
    println!("deleted hello.txt");
    control.delete_bucket().set_name(&bucket).send().await?;
    println!("deleted bucket {bucket_id}");
    Ok(())
}
