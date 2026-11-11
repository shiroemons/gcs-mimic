//! Uses the official Rust client against a local gcs-mimic.

use google_cloud_auth::credentials::anonymous;
use google_cloud_gax::retry_policy::RetryPolicyExt;
use google_cloud_storage::client::{Storage, StorageControl};
use google_cloud_storage::model::Bucket;
use google_cloud_storage::retry_policy::RetryableErrors;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    for suffix in ["ENDPOINT", "GRPC_ENDPOINT"] {
        let old = format!("GCS_MIMIC_{suffix}");
        let new = format!("GCSM_{suffix}");
        if std::env::var_os(&new).is_none() && std::env::var_os(&old).is_some() {
            return Err(format!("{old} was renamed to {new}").into());
        }
    }
    let endpoint =
        std::env::var("GCSM_ENDPOINT").unwrap_or_else(|_| "http://localhost:4443".into());
    let grpc_endpoint =
        std::env::var("GCSM_GRPC_ENDPOINT").unwrap_or_else(|_| "http://localhost:4444".into());

    // `Storage` reads and writes objects over HTTP. `StorageControl` manages buckets
    // and lists and deletes objects over gRPC, which gcs-mimic serves on its own port.
    // Anonymous credentials keep both clients away from Google sign-in. The default
    // retry policy keeps retrying for 5 minutes, so stop after 30 seconds instead
    // when the emulator is not running.
    let storage = Storage::builder()
        .with_endpoint(endpoint)
        .with_credentials(anonymous::Builder::new().build())
        .with_retry_policy(RetryableErrors.with_time_limit(Duration::from_secs(30)))
        .build()
        .await?;
    let control = StorageControl::builder()
        .with_endpoint(grpc_endpoint)
        .with_credentials(anonymous::Builder::new().build())
        .with_retry_policy(RetryableErrors.with_time_limit(Duration::from_secs(30)))
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
        .write_object(&bucket, "hello.txt", "Hello from gcs-mimic!")
        .send_buffered()
        .await?;
    println!("uploaded hello.txt");

    let listed = control.list_objects().set_parent(&bucket).send().await?;
    for object in listed.objects {
        println!("listed {} ({} bytes)", object.name, object.size);
    }

    let mut reader = storage.read_object(&bucket, "hello.txt").send().await?;
    let mut content = Vec::new();
    while let Some(chunk) = reader.next().await.transpose()? {
        content.extend_from_slice(&chunk);
    }
    println!("downloaded: {}", String::from_utf8(content)?);

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
