//! Workflows the Rust SDK exposes beyond the shared core: a rewrite bounded per call
//! (`StorageControl`, gRPC) and an unbuffered upload from a streaming source (JSON).

use super::copy::rewrite_request;
use crate::harness::{Ctx, Result, check, path};
use bytes::Bytes;
use google_cloud_storage::model::Object;
use google_cloud_storage::streaming_source::{Seek, StreamingSource};
use std::convert::Infallible;

const MIB: usize = 1024 * 1024;
/// Stops a rewrite that never reports completion.
const MAX_REWRITE_CALLS: u32 = 20;
const PIECE: u64 = 64 * 1024;

/// Position-dependent bytes, so a misplaced or repeated piece changes the content.
fn pattern_byte(index: u64) -> u8 {
    index.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_be_bytes()[0]
}

fn pattern(len: usize) -> Vec<u8> {
    (0_u64..).take(len).map(pattern_byte).collect()
}

/// Generates the pattern piece by piece, so the upload streams from a source that never
/// holds the whole payload. It gives no size hint, as a live stream could not.
struct PatternSource {
    len: u64,
    offset: u64,
}

impl StreamingSource for PatternSource {
    type Error = Infallible;

    async fn next(&mut self) -> Option<std::result::Result<Bytes, Infallible>> {
        if self.offset >= self.len {
            return None;
        }
        let end = (self.offset + PIECE).min(self.len);
        let piece: Vec<u8> = (self.offset..end).map(pattern_byte).collect();
        self.offset = end;
        Some(Ok(Bytes::from(piece)))
    }
}

// `send_unbuffered` rewinds the source after computing its checksum, and on a retry.
impl Seek for PatternSource {
    type Error = Infallible;

    async fn seek(&mut self, offset: u64) -> std::result::Result<(), Infallible> {
        self.offset = offset;
        Ok(())
    }
}

pub(super) async fn rewrite_token_loop(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let data = pattern(3 * MIB + 9);
    ctx.upload(&bucket, "src", &data).await?;
    // The per-call limit applies only when the location or storage class changes, so the
    // destination changes class. Each `rewrite_object` call is one RPC; the probe carries
    // the token from one response into the next request.
    let request = rewrite_request(ctx, (&bucket, "src"), (&bucket, "dst"))?
        .set_destination(Object::new().set_storage_class("NEARLINE"))
        .set_max_bytes_rewritten_per_call(i64::try_from(MIB)?);
    let mut token = String::new();
    let (mut calls, mut with_token) = (0_u32, 0_u32);
    let response = loop {
        with_token += u32::from(!token.is_empty());
        let response = request.clone().set_rewrite_token(token).send().await?;
        calls += 1;
        if response.done || calls == MAX_REWRITE_CALLS {
            break response;
        }
        token = response.rewrite_token;
    };
    check(
        response.done && with_token >= 2,
        format!(
            "done={} after {calls} calls, {with_token} with the rewrite token",
            response.done
        ),
    )?;
    check(
        response.total_bytes_rewritten == i64::try_from(data.len())?,
        format!("total bytes rewritten {}", response.total_bytes_rewritten),
    )?;
    let class = response
        .resource
        .map(|object| object.storage_class)
        .unwrap_or_default();
    check(
        class == "NEARLINE",
        format!("destination storage class {class:?}"),
    )?;
    check(ctx.read(&bucket, "dst").await? == data, "rewrite bytes")
}

pub(super) async fn write_unbuffered(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let len = 3 * MIB + 7;
    let source = PatternSource {
        len: u64::try_from(len)?,
        offset: 0,
    };
    let written = ctx
        .storage
        .write_object(path(&bucket), "streamed.bin", source)
        .send_unbuffered()
        .await?;
    check(
        written.size == i64::try_from(len)?,
        format!("written size {}", written.size),
    )?;
    check(
        ctx.read(&bucket, "streamed.bin").await? == pattern(len),
        "bytes differ",
    )
}
