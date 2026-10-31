use crate::config::{BoxError, Config};
use crate::transport::{self, SignedHttp};
use bytes::Bytes;
use google_cloud_auth::signer::Signer;
use google_cloud_gax::error::rpc::Code;
use google_cloud_storage::builder::storage::ReadObject;
use google_cloud_storage::client::{Storage, StorageControl};
use google_cloud_storage::model::{Bucket, ListObjectsResponse, Object};
use google_cloud_wkt::FieldMask;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) type Result<T> = std::result::Result<T, BoxError>;

pub(crate) struct Ctx {
    pub(crate) origin: String,
    pub(crate) project: String,
    pub(crate) storage: Storage,
    control: std::result::Result<StorageControl, String>,
    pub(crate) signer: Signer,
    pub(crate) http: SignedHttp,
    nonce: u64,
    counter: AtomicU64,
}

/// The error class a probe expects, mirroring the Python probe's exception types.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Expect {
    NotFound,
    PreconditionFailed,
    Forbidden,
    /// Any error returned by the service, as opposed to a transport or client failure.
    ServiceError,
}

impl Ctx {
    pub(crate) async fn new(config: &Config) -> Result<Self> {
        let grpc = config
            .grpc
            .as_deref()
            .ok_or_else(|| io::Error::other("SCORECARD_GRPC_ENDPOINT is required"))?;
        // A closed or unusable gRPC port fails each case that needs it, not the harness.
        let control = transport::control(grpc)
            .await
            .map_err(|error| transport::chain(error.as_ref()));
        let key = std::fs::read_to_string(&config.signing_key)?;
        let signer =
            google_cloud_auth::credentials::service_account::Builder::new(serde_json::json!({
                "type": "service_account",
                "client_email": "local-admin@example.invalid",
                "private_key_id": "scorecard-test-only",
                "project_id": config.project,
                "private_key": key,
                "token_uri": "https://oauth2.googleapis.com/token",
            }))
            .build_signer()?;
        Ok(Self {
            origin: config.origin.clone(),
            project: config.project.clone(),
            storage: transport::storage(config).await?,
            control,
            signer,
            http: SignedHttp::new(config)?,
            nonce: RandomState::new().build_hasher().finish(),
            counter: AtomicU64::new(0),
        })
    }

    pub(crate) fn control(&self) -> Result<&StorageControl> {
        self.control.as_ref().map_err(|error| {
            io::Error::other(format!("StorageControl unavailable: {error}")).into()
        })
    }

    pub(crate) fn unique(&self) -> String {
        let count = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(self.nonce ^ count);
        format!("sc-rs-{:012x}", hasher.finish() & 0xffff_ffff_ffff)
    }

    pub(crate) async fn create_bucket_named(&self, name: &str, bucket: Bucket) -> Result<String> {
        self.control()?
            .create_bucket()
            .set_parent("projects/_")
            .set_bucket_id(name)
            .set_bucket(bucket.set_project(format!("projects/{}", self.project)))
            .send()
            .await?;
        Ok(name.to_owned())
    }

    pub(crate) async fn create_bucket(&self, bucket: Bucket) -> Result<String> {
        self.create_bucket_named(&self.unique(), bucket).await
    }

    pub(crate) async fn bucket(&self) -> Result<String> {
        self.create_bucket(Bucket::new()).await
    }

    pub(crate) async fn get_bucket(&self, name: &str) -> Result<Bucket> {
        Ok(self
            .control()?
            .get_bucket()
            .set_name(path(name))
            .send()
            .await?)
    }

    pub(crate) async fn update_bucket(
        &self,
        name: &str,
        bucket: Bucket,
        paths: &[&str],
    ) -> Result<Bucket> {
        Ok(self
            .control()?
            .update_bucket()
            .set_bucket(bucket.set_name(path(name)))
            .set_update_mask(mask(paths))
            .send()
            .await?)
    }

    pub(crate) async fn upload(&self, bucket: &str, name: &str, data: &[u8]) -> Result<Object> {
        Ok(self
            .storage
            .write_object(path(bucket), name, Bytes::copy_from_slice(data))
            .send_buffered()
            .await?)
    }

    pub(crate) async fn seed(&self, bucket: &str, names: &[&str]) -> Result<()> {
        for name in names {
            self.upload(bucket, name, name.as_bytes()).await?;
        }
        Ok(())
    }

    pub(crate) fn reader(&self, bucket: &str, name: &str) -> ReadObject {
        self.storage.read_object(path(bucket), name)
    }

    pub(crate) async fn read(&self, bucket: &str, name: &str) -> Result<Vec<u8>> {
        Ok(collect(self.reader(bucket, name)).await?)
    }

    pub(crate) async fn get_object(&self, bucket: &str, name: &str) -> Result<Object> {
        Ok(get_object(self.control()?, bucket, name).await?)
    }

    pub(crate) async fn update_object(&self, object: Object, paths: &[&str]) -> Result<Object> {
        Ok(self
            .control()?
            .update_object()
            .set_object(object)
            .set_update_mask(mask(paths))
            .send()
            .await?)
    }

    pub(crate) async fn delete_object(
        &self,
        bucket: &str,
        name: &str,
    ) -> google_cloud_storage::Result<()> {
        let control = self.control().map_err(google_cloud_gax::error::Error::io)?;
        control
            .delete_object()
            .set_bucket(path(bucket))
            .set_object(name)
            .send()
            .await
    }

    /// Lists every page of an object listing, checking the page size on each page.
    pub(crate) async fn list_pages(
        &self,
        request: google_cloud_storage::builder::storage_control::ListObjects,
        page_limit: Option<usize>,
    ) -> Result<Vec<ListObjectsResponse>> {
        let mut pages = Vec::new();
        let mut token = String::new();
        loop {
            let page = request.clone().set_page_token(token).send().await?;
            if let Some(limit) = page_limit
                && page.objects.len() > limit
            {
                return Err(fail(format!("object page exceeds {limit}")));
            }
            token = page.next_page_token.clone();
            pages.push(page);
            if token.is_empty() {
                return Ok(pages);
            }
        }
    }

    pub(crate) async fn list_names(
        &self,
        request: google_cloud_storage::builder::storage_control::ListObjects,
    ) -> Result<Vec<String>> {
        let pages = self.list_pages(request, None).await?;
        Ok(sorted(
            pages
                .into_iter()
                .flat_map(|page| page.objects)
                .map(|object| object.name),
        ))
    }

    pub(crate) fn list_objects(
        &self,
        bucket: &str,
    ) -> Result<google_cloud_storage::builder::storage_control::ListObjects> {
        Ok(self.control()?.list_objects().set_parent(path(bucket)))
    }
}

pub(crate) async fn get_object(
    control: &StorageControl,
    bucket: &str,
    name: &str,
) -> google_cloud_storage::Result<Object> {
    control
        .get_object()
        .set_bucket(path(bucket))
        .set_object(name)
        .send()
        .await
}

pub(crate) async fn collect(reader: ReadObject) -> google_cloud_storage::Result<Vec<u8>> {
    let mut response = reader.send().await?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.next().await.transpose()? {
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub(crate) fn path(bucket: &str) -> String {
    format!("projects/_/buckets/{bucket}")
}

pub(crate) fn mask(paths: &[&str]) -> FieldMask {
    FieldMask::default().set_paths(paths.iter().copied())
}

pub(crate) fn sorted(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut names: Vec<String> = names.into_iter().collect();
    names.sort();
    names
}

pub(crate) fn fail(message: impl Into<String>) -> BoxError {
    io::Error::other(message.into()).into()
}

pub(crate) fn check(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(fail(message))
    }
}

pub(crate) fn expect_error<T>(result: google_cloud_storage::Result<T>, want: Expect) -> Result<()> {
    let error = match result {
        Ok(_) => return Err(fail(format!("expected {want:?}, got success"))),
        Err(error) => error,
    };
    let http = error.http_status_code();
    let code = error.status().map(|status| status.code);
    let matched = match want {
        Expect::NotFound => http == Some(404) || code == Some(Code::NotFound),
        Expect::PreconditionFailed => http == Some(412) || code == Some(Code::FailedPrecondition),
        Expect::Forbidden => http == Some(403) || code == Some(Code::PermissionDenied),
        Expect::ServiceError => http.is_some() || code.is_some(),
    };
    if matched {
        Ok(())
    } else {
        Err(fail(format!(
            "expected {want:?}, got {}",
            transport::chain(&error)
        )))
    }
}
