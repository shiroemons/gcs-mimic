//! Server-side copy probes: rewrite, compose and move through `StorageControl` (gRPC).

use crate::harness::{Ctx, Expect, Result, check, expect_error, path};
use google_cloud_storage::model::Object;
use google_cloud_storage::model::compose_object_request::SourceObject;

/// Rewrites `source` into `target` with the destination `properties`, repeating until
/// the service reports completion. The API requires the destination's bucket and name
/// in their own fields, not in `properties`.
async fn rewrite(
    ctx: &Ctx,
    source: (&str, &str),
    target: (&str, &str),
    properties: Object,
) -> Result<Object> {
    let mut token = String::new();
    loop {
        let response = ctx
            .control()?
            .rewrite_object()
            .set_source_bucket(path(source.0))
            .set_source_object(source.1)
            .set_destination_bucket(path(target.0))
            .set_destination_name(target.1)
            .set_destination(properties.clone())
            .set_rewrite_token(token)
            .send()
            .await?;
        if response.done {
            return response
                .resource
                .ok_or_else(|| crate::harness::fail("rewrite finished without a resource"));
        }
        token = response.rewrite_token;
    }
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
    .map_err(|_| crate::harness::fail("source remains"))?;
    check(ctx.read(&bucket, "new").await? == b"moved", "move bytes")
}
