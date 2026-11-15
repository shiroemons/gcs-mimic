//! Signed URL and POST policy probes. The SDK signs offline with the test service
//! account key for the scorecard origin; the generated URL and form are sent unchanged.

use crate::harness::{Ctx, Result, check, fail, path};
use google_cloud_storage::builder::storage::{PostPolicyV4Builder, SignedUrlBuilder};
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE, HeaderName, LOCATION};
use reqwest::{Method, Response, multipart};
use std::time::Duration;

const EXPIRATION: Duration = Duration::from_secs(5 * 60);
const REPORT_DISPOSITION: &str = r#"attachment; filename="report.csv""#;

fn url_builder(ctx: &Ctx, bucket: &str, object: &str, method: Method) -> SignedUrlBuilder {
    SignedUrlBuilder::for_object(path(bucket), object)
        .with_endpoint(ctx.origin.as_str())
        .with_method(method)
        .with_expiration(EXPIRATION)
}

async fn sign(ctx: &Ctx, bucket: &str, method: Method) -> Result<String> {
    Ok(url_builder(ctx, bucket, "signed.txt", method)
        .sign_with(&ctx.signer)
        .await?)
}

/// The value of `name` when the response carries it exactly once, as text.
fn single_header(response: &Response, name: HeaderName) -> Option<String> {
    let mut values = response.headers().get_all(name).iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value.to_str().ok().map(str::to_owned),
        _ => None,
    }
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

pub(super) async fn v4_resumable_upload(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let data: Vec<u8> = (0_u8..=250).cycle().take(256 * 1024 + 5).collect();
    // The signature covers the start header, so the request must send it unchanged.
    let url = url_builder(ctx, &bucket, "resumable.bin", Method::POST)
        .with_header("x-goog-resumable", "start")
        .sign_with(&ctx.signer)
        .await?;
    let start = ctx
        .http
        .request(Method::POST, &url)?
        .header("x-goog-resumable", "start")
        .build()?;
    let response = ctx.http.send(start).await?;
    let session = single_header(&response, LOCATION).unwrap_or_default();
    require_ok(response, |s| s == 201).await?;
    check(
        ctx.http.same_origin(&session),
        format!("session URI {session:?} leaves the origin"),
    )?;
    let finish = ctx
        .http
        .request(Method::PUT, &session)?
        .body(data.clone())
        .build()?;
    require_ok(ctx.http.send(finish).await?, |s| s == 200 || s == 201).await?;
    check(
        ctx.read(&bucket, "resumable.bin").await? == data,
        "signed resumable bytes",
    )
}

pub(super) async fn v4_response_overrides(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.storage
        .write_object(
            path(&bucket),
            "report.txt",
            bytes::Bytes::from_static(b"a,b\n"),
        )
        .set_content_type("text/plain")
        .send_buffered()
        .await?;
    let url = url_builder(ctx, &bucket, "report.txt", Method::GET)
        .with_query_param("response-content-disposition", REPORT_DISPOSITION)
        .with_query_param("response-content-type", "text/csv")
        .sign_with(&ctx.signer)
        .await?;
    let request = ctx.http.request(Method::GET, &url)?.build()?;
    let response = ctx.http.send(request).await?;
    let headers = (
        single_header(&response, CONTENT_DISPOSITION),
        single_header(&response, CONTENT_TYPE),
    );
    let body = require_ok(response, |s| s == 200).await?;
    check(body.as_ref() == b"a,b\n", "signed bytes")?;
    check(
        headers.0.as_deref() == Some(REPORT_DISPOSITION)
            && headers.1.as_deref() == Some("text/csv"),
        format!("override headers {headers:?}"),
    )?;
    let stored = ctx.get_object(&bucket, "report.txt").await?.content_type;
    check(
        stored == "text/plain",
        format!("stored content type {stored}"),
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
