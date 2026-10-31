//! Object write, read and metadata probes. Data moves through `Storage` (JSON API);
//! metadata reads and updates go through `StorageControl` (gRPC), the only public
//! metadata API in the Rust SDK.

use crate::harness::{Ctx, Expect, Result, check, collect, expect_error, fail, mask, path};
use crate::transport::chain;
use bytes::Bytes;
use google_cloud_gax::error::rpc::Code;
use google_cloud_storage::model::{Object, ObjectAccessControl};
use google_cloud_storage::model_ext::{KeyAes256, ReadRange};
use google_cloud_wkt::Timestamp;
use std::collections::HashMap;

/// MD5 of `md5 payload`, fixed so the probe needs no hashing dependency.
const MD5_PAYLOAD: [u8; 16] = [
    0x06, 0x2b, 0x07, 0xe3, 0x80, 0x96, 0xa1, 0x65, 0xc0, 0x14, 0x01, 0x2b, 0xc5, 0x11, 0xca, 0x40,
];

/// `gzip -n -9` of `"compress me " * 50`, fixed so the probe needs no compression
/// dependency.
const GZIPPED: [u8; 39] = [
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x4b, 0xce, 0xcf, 0x2d, 0x28, 0x4a,
    0x2d, 0x2e, 0x56, 0xc8, 0x4d, 0x55, 0x48, 0x1e, 0x65, 0x8f, 0xb2, 0xa9, 0xc4, 0x06, 0x00, 0xae,
    0x62, 0x71, 0x8d, 0x58, 0x02, 0x00, 0x00,
];

fn object_ref(bucket: &str, name: &str) -> Object {
    Object::new().set_bucket(path(bucket)).set_name(name)
}

pub(super) async fn write_read_small(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let name = "dir/日本語 +%?.txt";
    ctx.upload(&bucket, name, b"hello").await?;
    check(ctx.read(&bucket, name).await? == b"hello", "bytes differ")
}

pub(super) async fn resumable_large(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let data = vec![b'a'; 3 * 1024 * 1024 + 123];
    // A zero threshold forces the resumable protocol, in 256 KiB chunks like Python's.
    ctx.storage
        .write_object(path(&bucket), "big.bin", Bytes::from(data.clone()))
        .with_resumable_upload_threshold(0_usize)
        .with_resumable_upload_buffer_size(256 * 1024_usize)
        .send_buffered()
        .await?;
    let size = ctx.get_object(&bucket, "big.bin").await?.size;
    check(
        usize::try_from(size).ok() == Some(data.len()),
        "size differs",
    )?;
    check(ctx.read(&bucket, "big.bin").await? == data, "bytes differ")
}

pub(super) async fn metadata_on_write(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.storage
        .write_object(path(&bucket), "meta.txt", Bytes::from_static(b"x"))
        .set_content_type("text/plain")
        .set_cache_control("no-cache")
        .set_content_disposition("attachment")
        .set_content_language("ja")
        .set_metadata([("k", "v")])
        .send_buffered()
        .await?;
    let got = ctx.get_object(&bucket, "meta.txt").await?;
    let metadata: HashMap<String, String> = [("k".to_owned(), "v".to_owned())].into();
    check(
        (
            got.content_type.as_str(),
            got.cache_control.as_str(),
            got.content_disposition.as_str(),
            got.content_language.as_str(),
        ) == ("text/plain", "no-cache", "attachment", "ja")
            && got.metadata == metadata,
        "metadata differs",
    )
}

pub(super) async fn checksum_rejected(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    // A known CRC32C is sent with the upload; zero is wrong for this payload.
    let result = ctx
        .storage
        .write_object(
            path(&bucket),
            "crc.txt",
            Bytes::from_static(b"checksum payload"),
        )
        .with_known_crc32c(0_u32)
        .send_buffered()
        .await;
    let error = match result {
        Ok(_) => return Err(fail("wrong CRC32C accepted")),
        Err(error) => error,
    };
    let payload = error
        .http_payload()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .unwrap_or_default();
    let text = format!("{} {payload}", chain(&error)).to_lowercase();
    let bad_request = error.http_status_code() == Some(400)
        || error
            .status()
            .is_some_and(|s| s.code == Code::InvalidArgument);
    check(
        bad_request && (text.contains("checksum") || text.contains("crc32c")),
        text,
    )?;
    expect_error(
        crate::harness::get_object(ctx.control()?, &bucket, "crc.txt").await,
        Expect::NotFound,
    )
    .map_err(|_| fail("rejected upload left an object"))
}

pub(super) async fn md5_accepted(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.storage
        .write_object(path(&bucket), "md5.txt", Bytes::from_static(b"md5 payload"))
        .compute_md5()
        .send_buffered()
        .await?;
    let got = ctx.get_object(&bucket, "md5.txt").await?.checksums;
    check(
        got.is_some_and(|c| c.md5_hash.as_ref() == MD5_PAYLOAD),
        "MD5 differs",
    )
}

pub(super) async fn range_reads(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "range.txt", b"0123456789").await?;
    let read = |range| collect(ctx.reader(&bucket, "range.txt").set_read_range(range));
    check(
        read(ReadRange::segment(2, 4)).await? == b"2345",
        "closed range",
    )?;
    check(read(ReadRange::offset(7)).await? == b"789", "open range")?;
    check(read(ReadRange::tail(3)).await? == b"789", "suffix range")
}

pub(super) async fn update_metadata(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.storage
        .write_object(path(&bucket), "u.txt", Bytes::from_static(b"x"))
        .set_metadata([("a", "1"), ("b", "2")])
        .send_buffered()
        .await?;
    // Per-key masks delete `a` and add `c` while keeping `b`, like Python's `None` value.
    let patch = object_ref(&bucket, "u.txt")
        .set_content_type("text/csv")
        .set_metadata([("c", "3")]);
    ctx.update_object(patch, &["content_type", "metadata.a", "metadata.c"])
        .await?;
    let got = ctx.get_object(&bucket, "u.txt").await?;
    let want: HashMap<String, String> = [("b", "2"), ("c", "3")]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .into();
    check(
        got.content_type == "text/csv" && got.metadata == want,
        format!("{:?}", got.metadata),
    )
}

pub(super) async fn delete_not_found(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "d.txt", b"x").await?;
    ctx.delete_object(&bucket, "d.txt").await?;
    expect_error(
        crate::harness::get_object(ctx.control()?, &bucket, "d.txt").await,
        Expect::NotFound,
    )
    .map_err(|_| fail("deleted object visible"))?;
    expect_error(ctx.delete_object(&bucket, "d.txt").await, Expect::NotFound)
}

pub(super) async fn does_not_exist_precondition(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let write = |data: &'static [u8]| {
        ctx.storage
            .write_object(path(&bucket), "once.txt", Bytes::from_static(data))
            .set_if_generation_match(0)
            .send_buffered()
    };
    write(b"first").await?;
    expect_error(write(b"second").await, Expect::PreconditionFailed)?;
    check(
        ctx.read(&bucket, "once.txt").await? == b"first",
        "precondition changed bytes",
    )
}

pub(super) async fn generation_match_precondition(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let object = ctx.upload(&bucket, "g.txt", b"x").await?;
    let delete = |generation: i64| {
        Ok::<_, crate::config::BoxError>(
            ctx.control()?
                .delete_object()
                .set_bucket(path(&bucket))
                .set_object("g.txt")
                .set_if_generation_match(generation)
                .send(),
        )
    };
    expect_error(
        delete(object.generation + 1)?.await,
        Expect::PreconditionFailed,
    )?;
    delete(object.generation)?.await?;
    Ok(())
}

pub(super) async fn metageneration_match_precondition(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let object = ctx.upload(&bucket, "m.txt", b"x").await?;
    let result = ctx
        .control()?
        .update_object()
        .set_object(object_ref(&bucket, "m.txt").set_content_type("text/plain"))
        .set_update_mask(crate::harness::mask(&["content_type"]))
        .set_if_metageneration_match(object.metageneration + 5)
        .send()
        .await;
    expect_error(result, Expect::PreconditionFailed)
}

/// GetObject omits `acl` unless the read mask names it (the pinned storage.proto documents the
/// default as every field except `acl` and `owner`), so the probe asks for it.
async fn get_object_with_acl(
    ctx: &Ctx,
    bucket: &str,
    name: &str,
) -> Result<Vec<ObjectAccessControl>> {
    let object = ctx
        .control()?
        .get_object()
        .set_bucket(path(bucket))
        .set_object(name)
        .set_read_mask(mask(&["acl"]))
        .send()
        .await?;
    Ok(object.acl)
}

/// Appends a READER entry and saves the whole object ACL, as Python's `acl.save()`
/// does; the Rust SDK exposes object ACLs only as an object field.
pub(super) async fn acl(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "acl.txt", b"x").await?;
    let mut acl = get_object_with_acl(ctx, &bucket, "acl.txt").await?;
    acl.push(
        ObjectAccessControl::new()
            .set_entity("allUsers")
            .set_role("READER"),
    );
    ctx.update_object(object_ref(&bucket, "acl.txt").set_acl(acl), &["acl"])
        .await?;
    let acl = get_object_with_acl(ctx, &bucket, "acl.txt").await?;
    check(
        acl.iter()
            .any(|e| e.entity == "allUsers" && e.role == "READER"),
        "ACL missing",
    )
}

pub(super) async fn custom_time(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "ct.txt", b"x").await?;
    // 2026-01-02T03:04:05Z
    let want = Timestamp::clamp(1_767_323_045, 0);
    ctx.update_object(
        object_ref(&bucket, "ct.txt").set_custom_time(want),
        &["custom_time"],
    )
    .await?;
    let got = ctx.get_object(&bucket, "ct.txt").await?.custom_time;
    check(got == Some(want), "customTime differs")
}

pub(super) async fn storage_class_on_write(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.storage
        .write_object(path(&bucket), "cold", Bytes::from_static(b"x"))
        .set_storage_class("COLDLINE")
        .send_buffered()
        .await?;
    let got = ctx.get_object(&bucket, "cold").await?;
    check(got.storage_class == "COLDLINE", "storage class differs")
}

pub(super) async fn gzip_transcoding(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.storage
        .write_object(path(&bucket), "gz.txt", Bytes::from_static(&GZIPPED))
        .set_content_type("text/plain")
        .set_content_encoding("gzip")
        .send_buffered()
        .await?;
    let plain = "compress me ".repeat(50);
    let decompressed = collect(
        ctx.reader(&bucket, "gz.txt")
            .with_automatic_decompression(true),
    )
    .await?;
    check(decompressed == plain.as_bytes(), "decompressed bytes")?;
    // The SDK default asks the service not to transcode, returning the stored bytes.
    check(
        ctx.read(&bucket, "gz.txt").await? == GZIPPED,
        "compressed bytes",
    )
}

pub(super) async fn customer_supplied_key(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let key: Vec<u8> = (0_u8..32)
        .map(|i| i.wrapping_mul(37).wrapping_add(11))
        .collect();
    ctx.storage
        .write_object(path(&bucket), "csek.txt", Bytes::from_static(b"secret"))
        .set_key(KeyAes256::new(&key)?)
        .send_buffered()
        .await?;
    expect_error(
        collect(ctx.reader(&bucket, "csek.txt")).await,
        Expect::ServiceError,
    )?;
    let keyed = collect(
        ctx.reader(&bucket, "csek.txt")
            .set_key(KeyAes256::new(&key)?),
    )
    .await?;
    check(keyed == b"secret", "keyed bytes")
}
