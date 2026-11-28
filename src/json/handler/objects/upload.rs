//! JSON API simple (media) and multipart object uploads.

use super::{AclSource, Written, idempotency_token};
use crate::object::ObjectKey;
use crate::service::body::RequestBody as Incoming;
use crate::service::object_headers;
use crate::service::upload_drive::{
    CatalogWait, DriveError, UploadEnd, UploadSender, drive_upload, stream_upload, upload_channel,
};
use crate::{
    bucket::BucketAccess,
    csek::CustomerKey,
    json::ApiError,
    json::{self, UploadRequest},
    object::{ExpectedHashes, ObjectError, ObjectRecord, UploadChunk},
    service::context::ObjectCtx,
    storage::{ObjectUpload, ReplayInvocation, UploadCheck},
};
use hyper::{
    HeaderMap, Request,
    body::{Body, Bytes},
};
use std::future::Future;

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers.get("content-length")?.to_str().ok()?.parse().ok()
}

/// The replay identity of one upload: the client's idempotency token and the exact request a
/// retry must repeat to get the first response back.
struct Replay {
    token: Option<String>,
    request: String,
}

impl Replay {
    /// Reads the token and binds the request. The binding is stored hashed in
    /// `upload_replays.request`, so its format must not change.
    fn of(
        request: &Request<Incoming>,
        ctx: ObjectCtx<'_>,
        key: Option<CustomerKey>,
    ) -> Result<Self, ApiError> {
        Ok(Self {
            token: idempotency_token(request.headers())?,
            request: format!(
                "{}|{}|{:?}",
                request.uri(),
                ctx.state.api_base_url,
                key.map(CustomerKey::sha256)
            ),
        })
    }

    fn invocation(&self) -> ReplayInvocation<'_> {
        ReplayInvocation {
            token: self.token.as_deref(),
            request: &self.request,
        }
    }
}

pub(super) async fn insert(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    target: UploadRequest<String>,
) -> Result<Written, ApiError> {
    let UploadRequest {
        bucket,
        name,
        params,
        content_encoding,
    } = target;
    let mut metadata = crate::object::ObjectMetadataUpdate::default();
    json::declare_content_encoding(&mut metadata, content_encoding)?;
    let max_bytes = ctx.config.max_object_bytes();
    let content_type = object_headers::upload_content_type(request.headers())?;
    // A media upload verifies no declared hash; see `upload_content_type`.
    let expected = ExpectedHashes::default();
    let key = crate::csek::request_key(request.headers())?;
    let replay = Replay::of(&request, ctx, key)?;
    if content_length(request.headers()).is_some_and(|length| length > max_bytes) {
        return Err(ObjectError::TooLarge.into());
    }
    // Complete the advisory check before body-derived errors can race its result.
    let mut permit = ctx
        .await_catalog(ctx.state.catalog.wait_object_upload())
        .await?;
    ctx.await_catalog(ctx.state.catalog.check_upload_replay(
        bucket.as_str(),
        UploadCheck::Object {
            name: &name,
            conditions: params.conditions,
        },
        &access,
        replay.invocation(),
        &mut permit,
    ))
    .await?;
    let (sender, body) = upload_channel();
    let acl = AclSource::from(permit.replay_acl_snapshot());
    let upload = ctx.state.catalog.put_object_prechecked(
        ObjectUpload {
            key: ObjectKey::from_bucket(bucket.as_key(), &name),
            content_type,
            expected,
            conditions: params.conditions,
            max_bytes,
            metadata: crate::object::ObjectMetadataUpdate {
                customer_key_sha256: key.map(CustomerKey::sha256),
                acl: params.predefined_acl.into(),
                ..metadata
            },
            access,
        },
        body,
        permit,
    );
    let producer = async {
        stream_upload(request.into_body(), &sender, max_bytes, None)
            .await
            .map(|_| UploadEnd::Eof)
    };
    let record = drive_upload(CatalogWait::Upload(ctx), upload, &sender, producer)
        .await
        .map_err(DriveError::into_error::<ApiError>)?;
    Written::keyed(&record, ctx, params.projection, key, acl)
}

pub(super) async fn multipart(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    target: UploadRequest<Option<String>>,
) -> Result<Written, ApiError> {
    let UploadRequest {
        bucket,
        name,
        params,
        content_encoding,
    } = target;
    // Cloud Storage stored an `uploadType=multipart` request whose body is not multipart as a
    // media upload (recorded 200 for `text/plain`).
    if !crate::multipart::is_multipart_content_type(request.headers()) {
        let name = name.ok_or_else(ApiError::missing_object_name)?;
        let target = UploadRequest {
            bucket,
            name,
            params,
            content_encoding,
        };
        return insert(request, ctx, access, target).await;
    }
    let max_bytes = ctx.config.max_object_bytes();
    let catalog = &ctx.state.catalog;
    object_headers::upload_content_type(request.headers())?;
    let key = crate::csek::request_key(request.headers())?;
    let boundary = crate::multipart::multipart_boundary(request.headers())?;
    let replay = Replay::of(&request, ctx, key)?;
    if content_length(request.headers()).is_some_and(|length| {
        length > max_bytes.saturating_add(crate::json::MULTIPART_FRAMING_LIMIT)
    }) {
        return Err(ObjectError::TooLarge.into());
    }
    let mut permit = ctx.await_catalog(catalog.wait_object_upload()).await?;
    // The final name may be in metadata; check the bucket before reading any body.
    ctx.await_catalog(
        catalog.check_upload_replay(
            bucket.as_str(),
            name.as_deref()
                .map_or(UploadCheck::Bucket, |name| UploadCheck::Object {
                    name,
                    conditions: params.conditions,
                }),
            &access,
            replay.invocation(),
            &mut permit,
        ),
    )
    .await?;
    let mut multipart = json::RelatedUpload::new(request.into_body(), &boundary);
    let bytes = multipart.metadata().await?;
    let name_known = name.is_some();
    let json::MultipartMetadata {
        name,
        update: mut metadata,
        expected,
    } = json::multipart_metadata(&bytes, name, ExpectedHashes::default())?;
    json::declare_content_encoding(&mut metadata, content_encoding)?;
    crate::service::access::retention::authorize_upload(
        &ctx.state.catalog,
        &access,
        bucket.as_str(),
        &metadata,
    )
    .await?;
    metadata.customer_key_sha256 = key.map(CustomerKey::sha256);
    metadata.acl = params.predefined_acl.into();
    if !name_known {
        ctx.await_catalog(catalog.check_upload_replay(
            bucket.as_str(),
            UploadCheck::Object {
                name: &name,
                conditions: params.conditions,
            },
            &access,
            replay.invocation(),
            &mut permit,
        ))
        .await?;
    }
    let part_type = multipart.headers(json::PartKind::Media).await?;
    if let (Some(declared), Some(part)) = (metadata.content_type.value(), &part_type) {
        json::check_declared_content_type(declared, part)?;
    }
    // A part without a type stores none unless the metadata names one.
    let content_type = part_type.unwrap_or_default();
    let (sender, body) = upload_channel();
    let acl = AclSource::from(permit.replay_acl_snapshot());
    let upload = catalog.put_object_prechecked(
        ObjectUpload {
            key: ObjectKey::from_bucket(bucket.as_key(), &name),
            content_type,
            expected,
            conditions: params.conditions,
            max_bytes,
            metadata,
            access,
        },
        body,
        permit,
    );
    let record = stream_multipart(ctx, &mut multipart, upload, sender, expected).await?;
    Written::keyed(&record, ctx, params.projection, key, acl)
}

/// Streams the media part into `upload`, then hands over the checksums the trailing parts
/// declared, so the catalog verifies them before publishing.
async fn stream_multipart<B: Body<Data = Bytes> + Unpin>(
    ctx: ObjectCtx<'_>,
    multipart: &mut json::RelatedUpload<B>,
    upload: impl Future<Output = Result<ObjectRecord, ObjectError>>,
    sender: UploadSender,
    expected: ExpectedHashes,
) -> Result<ObjectRecord, ApiError> {
    let max_bytes = ctx.config.max_object_bytes();
    let streaming = async {
        let mut size = 0_u64;
        while let Some(bytes) = multipart.next_chunk().await? {
            size = size
                .checked_add(bytes.len() as u64)
                .filter(|size| *size <= max_bytes)
                .ok_or_else(|| ApiError::from(ObjectError::TooLarge))?;
            if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
                return Ok::<_, ApiError>(UploadEnd::EofWithHashes(expected));
            }
        }
        let expected = multipart.checksums(expected).await?;
        multipart.finish().await?;
        Ok(UploadEnd::EofWithHashes(expected))
    };
    drive_upload(CatalogWait::Upload(ctx), upload, &sender, streaming)
        .await
        .map_err(DriveError::into_error::<ApiError>)
}
