//! JSON API simple (media) and multipart object uploads.

use super::{Written, idempotency_token};
use crate::server::RequestBody as Incoming;
use crate::{
    bucket::BucketAccess,
    csek::CustomerKey,
    json::{self, JsonError, ObjectInsertParams},
    object::{ExpectedHashes, ObjectError, ObjectRecord, UploadChunk},
    server::{ObjectCtx, stream_upload},
    storage::{ObjectUpload, ReplayInvocation},
};
use http_body_util::BodyExt;
use hyper::{
    HeaderMap, Request,
    body::{Body, Bytes},
};
use std::{future::Future, io};
use tokio::sync::mpsc;

type ChunkSender = mpsc::Sender<Result<UploadChunk, io::Error>>;

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
}

/// What a replayed upload must repeat to return the first upload's response.
fn replay_binding(
    request: &Request<Incoming>,
    ctx: ObjectCtx<'_>,
    key: Option<CustomerKey>,
) -> String {
    format!(
        "{}|{}|{:?}",
        request.uri(),
        ctx.state.api_base_url,
        key.map(CustomerKey::sha256)
    )
}

pub(super) async fn insert(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    bucket: String,
    name: String,
    params: ObjectInsertParams,
) -> Result<Written, JsonError> {
    let max_bytes = ctx.config.max_object_bytes;
    let (content_type, expected) =
        json::parse_object_upload_headers(request.headers(), params.conditions)?;
    let key = json::request_key(request.headers())?;
    let token = idempotency_token(request.headers())?;
    let replay_request = replay_binding(&request, ctx, key);
    if content_length(request.headers()).is_some_and(|length| length > max_bytes) {
        return Err(ObjectError::TooLarge.into());
    }
    // Complete the advisory check before body-derived errors can race its result.
    let mut permit = ctx.state.catalog.admit_object_upload()?;
    ctx.await_catalog(ctx.state.catalog.check_upload_replay(
        &bucket,
        &name,
        params.conditions,
        &access,
        ReplayInvocation {
            token: token.as_deref(),
            request: &replay_request,
        },
        &mut permit,
    ))
    .await?;
    let (sender, body) = mpsc::channel(2);
    let acl_snapshot = permit.replay_acl_snapshot();
    let upload = ctx.state.catalog.put_object_prechecked(
        ObjectUpload {
            bucket: &bucket,
            name: &name,
            content_type,
            expected,
            conditions: params.conditions,
            max_bytes,
            metadata: crate::object::ObjectMetadataUpdate {
                customer_key_sha256: key.map(CustomerKey::sha256),
                ..Default::default()
            },
            access,
        },
        body,
        permit,
    );
    tokio::pin!(upload);
    let record = tokio::select! {
        result = &mut upload => result?,
        result = stream_upload(request.into_body(), &sender, max_bytes, ctx.activity, None) => {
            if let Err(error) = result {
                if sender.send(Err(io::Error::other(error.to_string()))).await.is_err() {
                    return Err(upload.await.err().unwrap_or(error).into());
                }
                return Err(error.into());
            }
            ctx.await_catalog(async {
                let _ = sender.send(Ok(UploadChunk::End)).await;
                drop(sender);
                upload.await
            }).await?
        }
    };
    Ok(Written {
        value: json::object_resource_keyed(
            &record,
            &ctx.endpoints.api_base_url,
            params.projection,
            key,
        )?,
        acl_snapshot,
    })
}

pub(super) async fn multipart(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    bucket: String,
    name: Option<String>,
    params: ObjectInsertParams,
) -> Result<Written, JsonError> {
    let max_bytes = ctx.config.max_object_bytes;
    let catalog = &ctx.state.catalog;
    let (_, expected) = json::parse_object_upload_headers(request.headers(), params.conditions)?;
    let key = json::request_key(request.headers())?;
    let boundary = json::multipart_boundary(request.headers())?;
    let token = idempotency_token(request.headers())?;
    let replay_request = replay_binding(&request, ctx, key);
    // The parser's JSON, header, delimiter-padding and discard caps total less than 256 KiB.
    if content_length(request.headers())
        .is_some_and(|length| length > max_bytes.saturating_add(256 * 1024))
    {
        return Err(ObjectError::TooLarge.into());
    }
    let mut permit = catalog.admit_object_upload()?;
    // The final name may be in metadata; check the bucket before reading any body.
    ctx.await_catalog(catalog.check_upload_replay(
        &bucket,
        name.as_deref().unwrap_or(""),
        if name.is_some() {
            params.conditions
        } else {
            Default::default()
        },
        &access,
        ReplayInvocation {
            token: token.as_deref(),
            request: &replay_request,
        },
        &mut permit,
    ))
    .await?;
    let activity = ctx.activity;
    let body = request.into_body().map_frame(|frame| {
        activity.send_modify(|progress| progress.awaiting_catalog = false);
        frame
    });
    let mut multipart = json::Multipart::new(body, &boundary);
    let bytes = multipart.metadata().await?;
    let name_known = name.is_some();
    let (name, mut metadata, expected) = json::multipart_metadata(&bytes, name, expected)?;
    crate::server::retention_access::authorize_upload(ctx.state, &access, &bucket, &metadata)
        .await?;
    metadata.customer_key_sha256 = key.map(CustomerKey::sha256);
    if !name_known {
        ctx.await_catalog(catalog.check_upload_replay(
            &bucket,
            &name,
            params.conditions,
            &access,
            ReplayInvocation {
                token: token.as_deref(),
                request: &replay_request,
            },
            &mut permit,
        ))
        .await?;
    }
    let content_type = multipart
        .headers(json::PartKind::Media)
        .await?
        .unwrap_or_else(|| "application/octet-stream".into());
    let (sender, body) = mpsc::channel(2);
    let acl_snapshot = permit.replay_acl_snapshot();
    let upload = catalog.put_object_prechecked(
        ObjectUpload {
            bucket: &bucket,
            name: &name,
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
    Ok(Written {
        value: json::object_resource_keyed(
            &record,
            &ctx.endpoints.api_base_url,
            params.projection,
            key,
        )?,
        acl_snapshot,
    })
}

/// Streams the media part into `upload`, then hands over the checksums the trailing parts
/// declared, so the catalog verifies them before publishing.
async fn stream_multipart<B: Body<Data = Bytes> + Unpin>(
    ctx: ObjectCtx<'_>,
    multipart: &mut json::Multipart<B>,
    upload: impl Future<Output = Result<ObjectRecord, ObjectError>>,
    sender: ChunkSender,
    expected: ExpectedHashes,
) -> Result<ObjectRecord, JsonError> {
    let max_bytes = ctx.config.max_object_bytes;
    tokio::pin!(upload);
    let streaming = async {
        let mut size = 0_u64;
        while let Some(bytes) = multipart.next_chunk().await? {
            size = size
                .checked_add(bytes.len() as u64)
                .filter(|size| *size <= max_bytes)
                .ok_or_else(|| JsonError::from(ObjectError::TooLarge))?;
            if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
                return Ok::<_, JsonError>(expected);
            }
        }
        let expected = multipart.checksums(expected).await?;
        multipart.finish().await?;
        Ok(expected)
    };
    let record = tokio::select! {
        result = &mut upload => result?,
        result = streaming => {
            let expected = match result {
                Ok(expected) => expected,
                Err(error) => {
                    let _ = sender.send(Err(io::Error::other(error.to_string()))).await;
                    drop(sender);
                    // Await cleanup before returning a framing error to the client.
                    match ctx.await_catalog(upload).await {
                        Ok(_) | Err(ObjectError::BodyAborted(_)) => {},
                        Err(storage_error) => return Err(storage_error.into()),
                    }
                    return Err(error);
                }
            };
            ctx.await_catalog(async {
                let _ = sender.send(Ok(UploadChunk::EndWithHashes(expected))).await;
                drop(sender);
                upload.await
            }).await?
        }
    };
    Ok(record)
}
