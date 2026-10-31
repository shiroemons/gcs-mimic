//! Signed URL and POST policy probes. The SDK signs offline with the test service
//! account key for the scorecard origin; the generated URL and form are sent unchanged.

use crate::harness::{Ctx, Result, check, fail, path};
use google_cloud_storage::builder::storage::{PostPolicyV4Builder, SignedUrlBuilder};
use reqwest::{Method, Response, multipart};
use std::time::Duration;

const EXPIRATION: Duration = Duration::from_secs(5 * 60);

async fn sign(ctx: &Ctx, bucket: &str, method: Method) -> Result<String> {
    Ok(SignedUrlBuilder::for_object(path(bucket), "signed.txt")
        .with_endpoint(ctx.origin.as_str())
        .with_method(method)
        .with_expiration(EXPIRATION)
        .sign_with(&ctx.signer)
        .await?)
}

async fn require_ok(response: Response, success: fn(u16) -> bool) -> Result<bytes::Bytes> {
    let status = response.status().as_u16();
    let body = response.bytes().await?;
    if success(status) {
        Ok(body)
    } else {
        let text = String::from_utf8_lossy(&body);
        Err(fail(format!(
            "status {status}: {}",
            text.chars().take(200).collect::<String>()
        )))
    }
}

pub(super) async fn v4_get(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "signed.txt", b"signed").await?;
    let url = sign(ctx, &bucket, Method::GET).await?;
    let request = ctx.http.request(Method::GET, &url)?.build()?;
    let body = require_ok(ctx.http.send(request).await?, |s| s == 200).await?;
    check(body.as_ref() == b"signed", "signed bytes")
}

pub(super) async fn v4_put(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let url = sign(ctx, &bucket, Method::PUT).await?;
    let request = ctx
        .http
        .request(Method::PUT, &url)?
        .body("uploaded")
        .build()?;
    require_ok(ctx.http.send(request).await?, |s| s == 200).await?;
    check(
        ctx.read(&bucket, "signed.txt").await? == b"uploaded",
        "signed bytes",
    )
}

pub(super) async fn post_policy_v4(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let policy = PostPolicyV4Builder::for_object(path(&bucket), "post.txt")
        .with_endpoint(ctx.origin.as_str())
        .with_expiration(EXPIRATION)
        .sign_with(&ctx.signer)
        .await?;
    let mut form = multipart::Form::new();
    for (name, value) in policy.fields {
        form = form.text(name, value);
    }
    let file = multipart::Part::bytes(b"posted".as_slice()).file_name("post.txt");
    let request = ctx
        .http
        .request(Method::POST, &policy.url)?
        .multipart(form.part("file", file))
        .build()?;
    require_ok(ctx.http.send(request).await?, |s| (200..300).contains(&s)).await?;
    check(
        ctx.read(&bucket, "post.txt").await? == b"posted",
        "posted bytes",
    )
}
