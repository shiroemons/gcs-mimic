//! Server-side copy probes: rewrite, compose and move through `StorageControl` (gRPC).

use super::object::test_key;
use crate::harness::{Ctx, Expect, Result, check, collect, expect_error, fail, path};
use bytes::Bytes;
use google_cloud_storage::builder::storage_control::RewriteObject;
use google_cloud_storage::model::bucket::Versioning;
use google_cloud_storage::model::compose_object_request::SourceObject;
use google_cloud_storage::model::{Bucket, CommonObjectRequestParams, Object};

/// A rewrite of `source` into `target`. The API requires the destination's bucket and
/// name in their own fields, not in the destination properties.
pub(super) fn rewrite_request(
    ctx: &Ctx,
    source: (&str, &str),
    target: (&str, &str),
) -> Result<RewriteObject> {
    Ok(ctx
        .control()?
        .rewrite_object()
        .set_source_bucket(path(source.0))
        .set_source_object(source.1)
        .set_destination_bucket(path(target.0))
        .set_destination_name(target.1))
}

/// Sends `request`, repeating it with each returned token until the service reports
/// completion.
async fn rewrite_to_end(request: RewriteObject) -> Result<Object> {
    let mut token = String::new();
    loop {
        let response = request.clone().set_rewrite_token(token).send().await?;
        if response.done {
            return response
                .resource
                .ok_or_else(|| fail("rewrite finished without a resource"));
        }
        token = response.rewrite_token;
    }
}

/// Rewrites `source` into `target` with the destination `properties`.
async fn rewrite(
    ctx: &Ctx,
    source: (&str, &str),
    target: (&str, &str),
    properties: Object,
) -> Result<Object> {
    rewrite_to_end(rewrite_request(ctx, source, target)?.set_destination(properties)).await
}

pub(super) async fn copy_with_metadata(ctx: &Ctx) -> Result<()> {
    let source = ctx.bucket().await?;
    let destination = ctx.bucket().await?;
    ctx.upload(&source, "src", b"copy me").await?;
    let properties = Object::new()
        .set_content_type("text/plain")
        .set_metadata([("copied", "yes")]);
    rewrite(ctx, (&source, "src"), (&destination, "dst"), properties).await?;
    let got = ctx.get_object(&destination, "dst").await?;
    check(
        got.content_type == "text/plain"
            && got.metadata.len() == 1
            && got.metadata.get("copied").map(String::as_str) == Some("yes"),
        "copy metadata",
    )?;
    check(
        ctx.read(&destination, "dst").await? == b"copy me",
        "copy bytes",
    )
}

pub(super) async fn rewrite_storage_class(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let data = vec![b'a'; 2 * 1024 * 1024 + 7];
    ctx.upload(&bucket, "src", &data).await?;
    let properties = Object::new().set_storage_class("NEARLINE");
    rewrite(ctx, (&bucket, "src"), (&bucket, "dst"), properties).await?;
    let got = ctx.get_object(&bucket, "dst").await?;
    check(got.storage_class == "NEARLINE", "storage class differs")?;
    check(ctx.read(&bucket, "dst").await? == data, "rewrite bytes")
}

pub(super) async fn compose(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let parts = ["p1", "p2", "p3"];
    ctx.seed(&bucket, &parts).await?;
    let composed = ctx
        .control()?
        .compose_object()
        .set_destination(Object::new().set_bucket(path(&bucket)).set_name("all"))
        .set_source_objects(parts.map(|name| SourceObject::new().set_name(name)))
        .send()
        .await?;
    check(composed.component_count == 3, "component count")?;
    check(
        ctx.read(&bucket, "all").await? == b"p1p2p3",
        "compose bytes",
    )
}

pub(super) async fn compose_preconditions(ctx: &Ctx) -> Result<()> {
    let bucket = ctx
        .create_bucket(Bucket::new().set_versioning(Versioning::new().set_enabled(true)))
        .await?;
    let first = ctx.upload(&bucket, "p1", b"one").await?.generation;
    ctx.upload(&bucket, "p1", b"ONE").await?;
    let second = ctx.upload(&bucket, "p2", b"two").await?.generation;
    // Each source's generation pins the version to concatenate, so p1 contributes `one`.
    let request = ctx
        .control()?
        .compose_object()
        .set_destination(Object::new().set_bucket(path(&bucket)).set_name("both"))
        .set_source_objects([
            SourceObject::new().set_name("p1").set_generation(first),
            SourceObject::new().set_name("p2").set_generation(second),
        ]);
    request.clone().send().await?;
    let composed = ctx.get_object(&bucket, "both").await?.generation;
    check(
        ctx.read(&bucket, "both").await? == b"onetwo",
        "compose bytes",
    )?;
    expect_error(
        request
            .clone()
            .set_if_generation_match(composed + 1)
            .send()
            .await,
        Expect::PreconditionFailed,
    )?;
    let after = ctx.get_object(&bucket, "both").await?.generation;
    check(
        after == composed,
        format!("destination generation changed from {composed} to {after}"),
    )?;
    check(
        ctx.read(&bucket, "both").await? == b"onetwo",
        "destination bytes changed",
    )?;
    request.set_if_generation_match(composed).send().await?;
    let replaced = ctx.get_object(&bucket, "both").await?.generation;
    check(
        replaced != composed,
        format!("matching compose kept generation {composed}"),
    )?;
    check(
        ctx.read(&bucket, "both").await? == b"onetwo",
        "matching compose bytes",
    )
}

pub(super) async fn csek_rotation(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let (old, new) = (test_key(23)?, test_key(59)?);
    ctx.storage
        .write_object(path(&bucket), "rotate.txt", Bytes::from_static(b"rotate"))
        .set_key(old.clone())
        .send_buffered()
        .await?;
    // RewriteObject takes the source key in its `copy_source_*` fields and has no setter
    // for a `KeyAes256`; the SDK's own conversion of the key into request parameters
    // supplies the algorithm and the SHA-256 those fields need.
    let source = CommonObjectRequestParams::from(old.clone());
    let request = rewrite_request(ctx, (&bucket, "rotate.txt"), (&bucket, "rotate.txt"))?
        .set_copy_source_encryption_algorithm(source.encryption_algorithm)
        .set_copy_source_encryption_key_bytes(source.encryption_key_bytes)
        .set_copy_source_encryption_key_sha256_bytes(source.encryption_key_sha256_bytes)
        .set_common_object_request_params(new.clone());
    rewrite_to_end(request).await?;
    let reader = || ctx.reader(&bucket, "rotate.txt");
    check(
        collect(reader().set_key(new)).await? == b"rotate",
        "new key bytes",
    )?;
    expect_error(collect(reader().set_key(old)).await, Expect::ServiceError)
        .map_err(|error| fail(format!("read with the old key: {error}")))?;
    expect_error(collect(reader()).await, Expect::ServiceError)
        .map_err(|error| fail(format!("read without a key: {error}")))
}

pub(super) async fn move_object(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "old", b"moved").await?;
    ctx.control()?
        .move_object()
        .set_bucket(path(&bucket))
        .set_source_object("old")
        .set_destination_object("new")
        .send()
        .await?;
    expect_error(
        crate::harness::get_object(ctx.control()?, &bucket, "old").await,
        Expect::NotFound,
    )
    .map_err(|_| fail("source remains"))?;
    check(ctx.read(&bucket, "new").await? == b"moved", "move bytes")
}
