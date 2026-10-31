//! Bucket probes. The Rust SDK manages buckets only through `StorageControl` (gRPC),
//! so each setting is written with an explicit field mask and read back with GetBucket.

use crate::harness::{Ctx, Expect, Result, check, expect_error, mask, path, sorted};
use google_cloud_iam_v1::model::{Binding, GetPolicyOptions};
use google_cloud_storage::model::bucket::iam_config::UniformBucketLevelAccess;
use google_cloud_storage::model::bucket::lifecycle::Rule;
use google_cloud_storage::model::bucket::lifecycle::rule::{Action, Condition};
use google_cloud_storage::model::bucket::{
    Autoclass, Cors, IamConfig, Lifecycle, SoftDeletePolicy, Versioning,
};
use google_cloud_storage::model::{Bucket, BucketAccessControl, ObjectAccessControl};
use std::collections::HashMap;

pub(super) async fn create_get_delete(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let got = ctx.get_bucket(&name).await?;
    check(got.name == path(&name), format!("bucket name {}", got.name))?;
    ctx.control()?
        .delete_bucket()
        .set_name(path(&name))
        .send()
        .await?;
    expect_error(
        ctx.control()?
            .get_bucket()
            .set_name(path(&name))
            .send()
            .await,
        Expect::NotFound,
    )
}

pub(super) async fn list_prefix_paging(ctx: &Ctx) -> Result<()> {
    let prefix = ctx.unique();
    for index in 0..3 {
        ctx.create_bucket_named(&format!("{prefix}-{index}"), Bucket::new())
            .await?;
    }
    let mut found = Vec::new();
    let mut token = String::new();
    loop {
        let page = ctx
            .control()?
            .list_buckets()
            .set_parent(format!("projects/{}", ctx.project))
            .set_prefix(&prefix)
            .set_page_size(2)
            .set_page_token(token)
            .send()
            .await?;
        check(page.buckets.len() <= 2, "bucket page exceeds 2")?;
        found.extend(page.buckets.into_iter().map(|bucket| {
            bucket
                .name
                .strip_prefix("projects/_/buckets/")
                .unwrap_or(&bucket.name)
                .to_owned()
        }));
        token = page.next_page_token;
        if token.is_empty() {
            break;
        }
    }
    let found = sorted(found);
    let want: Vec<String> = (0..3).map(|index| format!("{prefix}-{index}")).collect();
    check(found == want, format!("{found:?}"))
}

pub(super) async fn update_labels(ctx: &Ctx) -> Result<()> {
    let name = ctx
        .create_bucket(Bucket::new().set_labels([("a", "1"), ("b", "2")]))
        .await?;
    // The `labels` mask replaces the whole map, which removes `a`.
    ctx.update_bucket(
        &name,
        Bucket::new().set_labels([("b", "2"), ("c", "3")]),
        &["labels"],
    )
    .await?;
    let labels = ctx.get_bucket(&name).await?.labels;
    let want: HashMap<String, String> = [("b", "2"), ("c", "3")]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .into();
    check(labels == want, format!("{labels:?}"))
}

pub(super) async fn metageneration_precondition(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let result = ctx
        .control()?
        .update_bucket()
        .set_bucket(
            Bucket::new()
                .set_name(path(&name))
                .set_storage_class("NEARLINE"),
        )
        .set_update_mask(mask(&["storage_class"]))
        .set_if_metageneration_match(999)
        .send()
        .await;
    expect_error(result, Expect::PreconditionFailed)
}

pub(super) async fn versioning_history(ctx: &Ctx) -> Result<()> {
    let name = ctx
        .create_bucket(Bucket::new().set_versioning(Versioning::new().set_enabled(true)))
        .await?;
    let first = ctx.upload(&name, "v.txt", b"one").await?;
    ctx.upload(&name, "v.txt", b"two").await?;
    let pages = ctx
        .list_pages(ctx.list_objects(&name)?.set_versions(true), None)
        .await?;
    let count: usize = pages.iter().map(|page| page.objects.len()).sum();
    check(count == 2, "version count")?;
    let old = crate::harness::collect(ctx.reader(&name, "v.txt").set_generation(first.generation))
        .await?;
    check(old == b"one", "old bytes")
}

pub(super) async fn lifecycle(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let rule = Rule::new()
        .set_action(Action::new().set_type("Delete"))
        .set_condition(Condition::new().set_age_days(30));
    ctx.update_bucket(
        &name,
        Bucket::new().set_lifecycle(Lifecycle::new().set_rule([rule])),
        &["lifecycle"],
    )
    .await?;
    let rules = ctx
        .get_bucket(&name)
        .await?
        .lifecycle
        .map(|l| l.rule)
        .unwrap_or_default();
    let age = rules
        .first()
        .and_then(|rule| rule.condition.as_ref())
        .and_then(|c| c.age_days);
    check(rules.len() == 1 && age == Some(30), format!("{rules:?}"))
}

pub(super) async fn cors(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let cors = Cors::new()
        .set_origin(["https://app.example"])
        .set_method(["GET"])
        .set_max_age_seconds(3600);
    ctx.update_bucket(&name, Bucket::new().set_cors([cors]), &["cors"])
        .await?;
    let got = ctx.get_bucket(&name).await?.cors;
    check(
        got.len() == 1
            && got
                .first()
                .is_some_and(|c| c.origin == ["https://app.example"]),
        format!("{got:?}"),
    )
}

pub(super) async fn uniform_bucket_level_access(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let config = IamConfig::new()
        .set_uniform_bucket_level_access(UniformBucketLevelAccess::new().set_enabled(true));
    ctx.update_bucket(
        &name,
        Bucket::new().set_iam_config(config),
        &["iam_config.uniform_bucket_level_access"],
    )
    .await?;
    let got = ctx.get_bucket(&name).await?.iam_config;
    let enabled = got
        .and_then(|c| c.uniform_bucket_level_access)
        .is_some_and(|u| u.enabled);
    check(enabled, "UBLA not enabled")
}

pub(super) async fn public_access_prevention(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let config = IamConfig::new().set_public_access_prevention("enforced");
    ctx.update_bucket(
        &name,
        Bucket::new().set_iam_config(config),
        &["iam_config.public_access_prevention"],
    )
    .await?;
    let got = ctx.get_bucket(&name).await?.iam_config;
    check(
        got.is_some_and(|c| c.public_access_prevention == "enforced"),
        "PAP not enforced",
    )
}

pub(super) async fn iam_policy(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let control = ctx.control()?;
    let get = || {
        control
            .get_iam_policy()
            .set_resource(path(&name))
            .set_options(GetPolicyOptions::new().set_requested_policy_version(3))
            .send()
    };
    let mut policy = get().await?;
    policy.bindings.push(
        Binding::new()
            .set_role("roles/storage.objectViewer")
            .set_members(["user:someone@example.invalid"]),
    );
    control
        .set_iam_policy()
        .set_resource(path(&name))
        .set_policy(policy)
        .send()
        .await?;
    let policy = get().await?;
    check(
        policy.bindings.iter().any(|binding| {
            binding.role == "roles/storage.objectViewer"
                && binding
                    .members
                    .iter()
                    .any(|member| member == "user:someone@example.invalid")
        }),
        "missing binding",
    )
}

pub(super) async fn iam_test_permissions(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let granted = ctx
        .control()?
        .test_iam_permissions()
        .set_resource(path(&name))
        .set_permissions(["storage.buckets.get", "storage.objects.list"])
        .send()
        .await?;
    check(!granted.permissions.is_empty(), "no permissions")
}

pub(super) async fn soft_delete_policy(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let policy = SoftDeletePolicy::new()
        .set_retention_duration(google_cloud_wkt::Duration::clamp(864_000, 0));
    ctx.update_bucket(
        &name,
        Bucket::new().set_soft_delete_policy(policy),
        &["soft_delete_policy"],
    )
    .await?;
    let got = ctx.get_bucket(&name).await?.soft_delete_policy;
    let seconds = got.and_then(|p| p.retention_duration).map(|d| d.seconds());
    check(seconds == Some(864_000), "soft delete policy differs")
}

pub(super) async fn autoclass(ctx: &Ctx) -> Result<()> {
    let name = ctx
        .create_bucket(Bucket::new().set_autoclass(Autoclass::new().set_enabled(true)))
        .await?;
    let got = ctx.get_bucket(&name).await?.autoclass;
    check(got.is_some_and(|a| a.enabled), "autoclass not enabled")
}

/// Appends a READER entry to the bucket ACL and saves the whole list, as the Python
/// SDK's `acl.save()` does; the Rust SDK exposes bucket ACLs only as a bucket field.
pub(super) async fn acl(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let mut acl = ctx.get_bucket(&name).await?.acl;
    acl.push(
        BucketAccessControl::new()
            .set_entity("allAuthenticatedUsers")
            .set_role("READER"),
    );
    ctx.update_bucket(&name, Bucket::new().set_acl(acl), &["acl"])
        .await?;
    let acl = ctx.get_bucket(&name).await?.acl;
    check(
        acl.iter()
            .any(|e| e.entity == "allAuthenticatedUsers" && e.role == "READER"),
        "ACL missing",
    )
}

pub(super) async fn default_object_acl(ctx: &Ctx) -> Result<()> {
    let name = ctx.bucket().await?;
    let mut acl = ctx.get_bucket(&name).await?.default_object_acl;
    acl.push(
        ObjectAccessControl::new()
            .set_entity("allAuthenticatedUsers")
            .set_role("READER"),
    );
    ctx.update_bucket(
        &name,
        Bucket::new().set_default_object_acl(acl),
        &["default_object_acl"],
    )
    .await?;
    let acl = ctx.get_bucket(&name).await?.default_object_acl;
    check(
        acl.iter()
            .any(|e| e.entity == "allAuthenticatedUsers" && e.role == "READER"),
        "ACL missing",
    )
}
