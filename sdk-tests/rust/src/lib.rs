use google_cloud_auth::credentials::anonymous;
use google_cloud_storage::{client::StorageControl, model::Bucket};
use std::{error::Error, io, time::SystemTime};
use url::Url;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

pub fn local_origin(raw: &str) -> Result<Url> {
    let origin = Url::parse(raw)?;
    if origin.scheme() != "http"
        || !matches!(origin.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || origin.port().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(io::Error::other(
            "endpoint must be a bare loopback HTTP origin with an explicit port",
        )
        .into());
    }
    Ok(origin)
}

pub fn guarded_origin() -> Result<Url> {
    let endpoint = local_origin(&std::env::var("GCS_MIMIC_ENDPOINT")?)?;
    let proxy = local_origin(&std::env::var("GCS_MIMIC_EGRESS_PROXY")?)?;
    if std::env::var("HTTP_PROXY")? != proxy.as_str().trim_end_matches('/')
        || !std::env::var("NO_PROXY")?.is_empty()
    {
        return Err(io::Error::other("run this executable through go/cmd/sdk-guard").into());
    }
    Ok(endpoint)
}

pub async fn control(endpoint: &Url) -> Result<StorageControl> {
    Ok(StorageControl::builder()
        .with_endpoint(endpoint.as_str().trim_end_matches('/'))
        .with_credentials(anonymous::Builder::new().build())
        .build()
        .await?)
}

pub async fn create_bucket(client: &StorageControl, label: &str) -> Result<String> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let bucket = format!("sdk-rust-{label}-{}-{nonce}", std::process::id());
    client
        .create_bucket()
        .set_parent("projects/_")
        .set_bucket_id(&bucket)
        .set_bucket(
            Bucket::new()
                .set_project("projects/local-project")
                .set_location("US"),
        )
        .send()
        .await?;
    Ok(bucket)
}

pub async fn cleanup(client: &StorageControl, bucket: &str, objects: &[&str]) -> Result<()> {
    let parent = format!("projects/_/buckets/{bucket}");
    let listed = client.list_objects().set_parent(&parent).send().await?;
    for object in listed.objects {
        if !objects.contains(&object.name.as_str()) {
            return Err(io::Error::other(
                "unexpected object in isolated SDK bucket; refusing cleanup",
            )
            .into());
        }
        client
            .delete_object()
            .set_bucket(&parent)
            .set_object(object.name)
            .send()
            .await?;
    }
    client.delete_bucket().set_name(parent).send().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::local_origin;

    #[test]
    fn endpoint_guard_rejects_cloud_credentials_paths_and_changed_origins() {
        assert!(local_origin("http://127.0.0.1:4443").is_ok());
        for input in [
            "https://storage.googleapis.com",
            "http://metadata.google.internal:80",
            "http://user@127.0.0.1:4443",
            "http://127.0.0.1:4443/path",
            "http://127.0.0.1:4443?x",
            "http://127.0.0.1",
        ] {
            assert!(local_origin(input).is_err(), "accepted {input}");
        }
    }
}
