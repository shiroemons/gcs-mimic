use futures_util::TryStreamExt;
use gcs_mimic_rust_sdk_scenarios::{Result, cleanup, control, create_bucket, guarded_origin};
use object_store::{
    ClientOptions, ObjectStore, ObjectStoreExt, gcp::GoogleCloudStorageBuilder, path::Path,
};
use std::io;

#[tokio::main]
async fn main() -> Result<()> {
    let endpoint = guarded_origin()?;
    let control = control(&endpoint).await?;
    let bucket = create_bucket(&control, "arrow").await?;
    let store = GoogleCloudStorageBuilder::new()
        .with_bucket_name(&bucket)
        .with_base_url(endpoint.as_str().trim_end_matches('/'))
        // PUT still requests credentials when skip_signature is enabled in this release.
        .with_service_account_key(
            r#"{"private_key":"","private_key_id":"","client_email":"","disable_oauth":true}"#,
        )
        .with_skip_signature(true)
        .with_client_options(
            ClientOptions::new()
                .with_allow_http(true)
                .with_proxy_url(std::env::var("GCS_MIMIC_EGRESS_PROXY")?)
                .with_proxy_excludes(""),
        )
        .build()?;
    let source = Path::parse("dir/日本語 +%?.txt")?;
    let copy = Path::from("dir/copy.txt");
    let result: Result<()> = async {
        let payload = b"Arrow object_store bytes";
        store.put(&source, payload.to_vec().into()).await?;
        if store.get(&source).await?.bytes().await?.as_ref() != payload {
            return Err(io::Error::other("Arrow bytes differ").into());
        }
        if store.get_range(&source, 0..5).await?.as_ref() != b"Arrow" {
            return Err(io::Error::other("Arrow range differs").into());
        }
        store.copy_if_not_exists(&source, &copy).await?;
        if !matches!(
            store.copy_if_not_exists(&source, &copy).await,
            Err(object_store::Error::AlreadyExists { .. })
        ) {
            return Err(io::Error::other("Arrow copy-if-absent overwrote a destination").into());
        }
        let mut listed = store.list(Some(&Path::from("dir")));
        let mut count = 0;
        while listed.try_next().await?.is_some() {
            count += 1;
        }
        if count != 2 {
            return Err(io::Error::other("Arrow XML listing differs").into());
        }
        println!("arrow-object-store: XML put, get, range, list, copy conditions PASS");
        Ok(())
    }
    .await;
    let cleanup_result = cleanup(&control, &bucket, &[source.as_ref(), copy.as_ref()]).await;
    result.and(cleanup_result)
}
