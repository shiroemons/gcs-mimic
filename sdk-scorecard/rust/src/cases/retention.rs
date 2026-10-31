//! Retention, holds and soft-delete probes: each one checks that the setting is
//! enforced, not only stored.

use crate::harness::{Ctx, Expect, Result, check, expect_error, path};
use google_cloud_storage::model::bucket::{RetentionPolicy, SoftDeletePolicy};
use google_cloud_storage::model::object::{Retention, retention::Mode};
use google_cloud_storage::model::{Bucket, Object};
use google_cloud_wkt::{Duration, Timestamp};
use std::time::{SystemTime, UNIX_EPOCH};

fn one_hour_policy() -> Bucket {
    Bucket::new().set_retention_policy(
        RetentionPolicy::new().set_retention_duration(Duration::clamp(3600, 0)),
    )
}

pub(super) async fn bucket_policy(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    ctx.update_bucket(&name, one_hour_policy(), &["retention_policy"])
        .await?;
    ctx.upload(&name, "r", b"r").await?;
    expect_error(ctx.delete_object(&name, "r").await, Expect::Forbidden)
}

pub(super) async fn bucket_lock(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    ctx.update_bucket(&name, one_hour_policy(), &["retention_policy"])
        .await?;
    let current = ctx.get_bucket(&name).await?;
    ctx.control()?
        .lock_bucket_retention_policy()
        .set_bucket(path(&name))
        .set_if_metageneration_match(current.metageneration)
        .send()
        .await?;
    let locked = ctx
        .get_bucket(&name)
        .await?
        .retention_policy
        .is_some_and(|p| p.is_locked);
    check(locked, "policy not locked")
}

fn object_ref(bucket: &str, name: &str) -> Object {
    Object::new().set_bucket(path(bucket)).set_name(name)
}

pub(super) async fn holds(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    ctx.upload(&bucket, "hold.txt", b"x").await?;
    ctx.update_object(
        object_ref(&bucket, "hold.txt").set_temporary_hold(true),
        &["temporary_hold"],
    )
    .await?;
    expect_error(
        ctx.delete_object(&bucket, "hold.txt").await,
        Expect::Forbidden,
    )?;
    ctx.update_object(
        object_ref(&bucket, "hold.txt").set_temporary_hold(false),
        &["temporary_hold"],
    )
    .await?;
    ctx.delete_object(&bucket, "hold.txt").await?;
    Ok(())
}

pub(super) async fn object_retention(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.unique();
    ctx.control()?
        .create_bucket()
        .set_parent("projects/_")
        .set_bucket_id(&bucket)
        .set_bucket(Bucket::new().set_project(format!("projects/{}", ctx.project)))
        .set_enable_object_retention(true)
        .send()
        .await?;
    ctx.upload(&bucket, "ret.txt", b"x").await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let until = Timestamp::clamp(i64::try_from(now)? + 3600, 0);
    let retention = Retention::new()
        .set_mode(Mode::Unlocked)
        .set_retain_until_time(until);
    ctx.update_object(
        object_ref(&bucket, "ret.txt").set_retention(retention),
        &["retention"],
    )
    .await?;
    let got = ctx.get_object(&bucket, "ret.txt").await?.retention;
    check(
        got.is_some_and(|r| r.mode == Mode::Unlocked),
        "retention not stored",
    )?;
    expect_error(
        ctx.delete_object(&bucket, "ret.txt").await,
        Expect::Forbidden,
    )
}

pub(super) async fn soft_delete_restore(ctx: &Ctx) -> Result<()> {
    let bucket = ctx.bucket().await?;
    let policy = SoftDeletePolicy::new().set_retention_duration(Duration::clamp(604_800, 0));
    ctx.update_bucket(
        &bucket,
        Bucket::new().set_soft_delete_policy(policy),
        &["soft_delete_policy"],
    )
    .await?;
    let object = ctx.upload(&bucket, "sd.txt", b"restore me").await?;
    ctx.delete_object(&bucket, "sd.txt").await?;
    let names = ctx
        .list_names(ctx.list_objects(&bucket)?.set_soft_deleted(true))
        .await?;
    check(names == ["sd.txt"], "soft-deleted listing")?;
    ctx.control()?
        .restore_object()
        .set_bucket(path(&bucket))
        .set_object("sd.txt")
        .set_generation(object.generation)
        .send()
        .await?;
    check(
        ctx.read(&bucket, "sd.txt").await? == b"restore me",
        "restored bytes",
    )
}
