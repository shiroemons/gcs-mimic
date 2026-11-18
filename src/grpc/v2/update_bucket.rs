//! Bucket metadata updates: UpdateBucket and LockBucketRetentionPolicy.

use super::{Storage, convert, mask::mask, pb, status::unsupported};
use crate::{
    auth::{self, Caller},
    bucket::{
        BucketAccess, BucketConditions, BucketMetadataUpdate, BucketRecord, CatalogError,
        RetentionPolicyUpdate,
    },
    grpc::validate::strict_bucket_conditions,
};
use tonic::{Request, Response, Status};

/// How often an update tries when the bucket changes between the lifecycle check and the write.
const LIFECYCLE_CHECK_ATTEMPTS: usize = 3;

impl Storage {
    pub(super) async fn lock_bucket_retention_policy(
        &self,
        request: Request<pb::LockBucketRetentionPolicyRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let r = request.into_inner();
        let bucket = self.bucket_id(&r.bucket)?;
        let conditions = strict_bucket_conditions(BucketConditions::from(&r))?;
        let access = self.access(caller, bucket, auth::BUCKETS_UPDATE).await?;
        let record = self
            .patch_bucket_with_lifecycle_response(
                bucket,
                &BucketMetadataUpdate {
                    retention_policy: RetentionPolicyUpdate::Lock,
                    ..Default::default()
                },
                conditions,
                &access,
            )
            .await?;
        let mut result = convert::bucket(&record, convert::Lifecycle::Include)?;
        self.add_bucket_acl(caller, &mut result).await?;
        Ok(Response::new(result))
    }
    async fn check_lifecycle_response(
        &self,
        bucket: &str,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<i64, Status> {
        let record = self
            .state
            .catalog
            .get_bucket(bucket, access)
            .await?
            .ok_or_else(|| Status::from(CatalogError::NotFound))?;
        conditions
            .check(record.metageneration)
            .map_err(super::bucket_update::input_error)?;
        convert::lifecycle(&record.lifecycle)?;
        Ok(record.metageneration)
    }

    async fn patch_bucket_with_lifecycle_response(
        &self,
        bucket: &str,
        update: &BucketMetadataUpdate,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<BucketRecord, Status> {
        let implicit_match =
            conditions.if_metageneration_match.is_none() && update.lifecycle.is_none();
        for _ in 0..LIFECYCLE_CHECK_ATTEMPTS {
            let mut checked = conditions;
            if update.lifecycle.is_none() {
                let metageneration = self
                    .check_lifecycle_response(bucket, conditions, access)
                    .await?;
                // shortcut: same-name recreation can reuse metageneration; add an incarnation guard to cover that race.
                checked
                    .if_metageneration_match
                    .get_or_insert(metageneration);
                #[cfg(test)]
                tests::write_after_lifecycle_check(self, bucket).await;
            }
            match self
                .state
                .catalog
                .patch_bucket(bucket, update.clone(), checked, access)
                .await
            {
                Err(CatalogError::PreconditionFailed) if implicit_match => {}
                result => return result.map_err(super::bucket_update::input_error),
            }
        }
        Err(Status::aborted(
            "Bucket changed during lifecycle validation; retry the update",
        ))
    }

    pub(super) async fn update_bucket(
        &self,
        request: Request<pb::UpdateBucketRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Bucket>, Status> {
        let mut r = request.into_inner();
        let b = r
            .bucket
            .take()
            .ok_or_else(|| Status::invalid_argument("bucket is required; supply bucket.name"))?;
        let bucket = self.bucket_id(&b.name)?;
        if !r.predefined_acl.is_empty() || !r.predefined_default_object_acl.is_empty() {
            return Err(unsupported("predefined bucket ACLs; omit predefined_acl"));
        }
        // Cloud Storage compares whatever conditions arrive, as GetBucket and DeleteBucket do:
        // a zero or negative match never holds, a zero or negative not-match always does, and
        // both may come together. It ignores the ETag in `bucket` (recorded 2026-11-18).
        let conditions = BucketConditions::from(&r);
        let paths = mask(r.update_mask.as_ref())?;
        let update = super::bucket_update::from_proto(&b, paths)?;
        let access = self.access(caller, bucket, auth::BUCKETS_UPDATE).await?;
        let record = self
            .patch_bucket_with_lifecycle_response(bucket, &update, conditions, &access)
            .await?;
        let mut result = convert::bucket(&record, convert::Lifecycle::Include)?;
        self.add_bucket_acl(caller, &mut result).await?;
        Ok(Response::new(result))
    }
}

#[cfg(test)]
mod tests {
    use super::super::bucket_update::from_proto as bucket_patch;
    use super::super::object_update::from_proto as object_patch;
    use super::super::test_support::{RESOURCE, setup, upload};
    use super::*;
    use crate::grpc::test_support::{self as fixtures, assert_code, assert_unsupported};
    use crate::grpc::validate::strict_conditions;
    use crate::json::{self, JsonError};
    use crate::object::{MapPatch, ObjectConditions};
    use crate::storage::test_fixtures::PERMISSIVE;
    use serde_json::{Value, json};
    use std::{cell::RefCell, collections::VecDeque};
    use tonic::Code;

    tokio::task_local! {
        static BUCKET_PRECHECK_UPDATES: RefCell<VecDeque<Value>>;
    }

    pub(super) async fn write_after_lifecycle_check(storage: &Storage, bucket: &str) {
        let Ok(Some(body)) =
            BUCKET_PRECHECK_UPDATES.try_with(|writes| writes.borrow_mut().pop_front())
        else {
            return;
        };
        let update =
            json::parse_bucket_patch(&serde_json::to_vec(&body).unwrap(), storage.compat_profile)
                .unwrap();
        storage
            .state
            .catalog
            .patch_bucket(bucket, update, BucketConditions::default(), PERMISSIVE)
            .await
            .unwrap();
    }

    fn lifecycle_label_update(conditions: BucketConditions) -> pb::UpdateBucketRequest {
        pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: "projects/_/buckets/update-copy-bucket".into(),
                labels: [("changed".into(), "yes".into())].into(),
                ..Default::default()
            }),
            if_metageneration_match: conditions.if_metageneration_match,
            if_metageneration_not_match: conditions.if_metageneration_not_match,
            update_mask: fixtures::mask(&["labels"]),
            ..Default::default()
        }
    }

    async fn stored_bucket(s: &Storage) -> BucketRecord {
        s.state
            .catalog
            .get_bucket("update-copy-bucket", PERMISSIVE)
            .await
            .unwrap()
            .unwrap()
    }

    #[test]
    fn grpc_bucket_masks_preserve_resets_and_omitted_parent_fields() {
        let bucket = pb::Bucket::default();
        let paths = [
            "autoclass",
            "versioning",
            "retention_policy",
            "soft_delete_policy",
            "cors",
            "iam_config.public_access_prevention",
            "iam_config.uniform_bucket_level_access",
            "iam_config",
        ]
        .map(String::from);
        let update = bucket_patch(&bucket, &paths).unwrap();
        assert_eq!(update.autoclass.unwrap().enabled, Some(false));
        assert_eq!(
            update.versioning,
            Some(crate::bucket::VersioningUpdate::Set(
                crate::bucket::Versioning::Disabled
            ))
        );
        assert_eq!(update.retention_policy, RetentionPolicyUpdate::Remove);
        assert_eq!(
            update
                .soft_delete_retention
                .map(|retention| retention.seconds()),
            Some(0)
        );
        assert!(update.cors.unwrap().is_empty());
        assert_eq!(
            update.public_access_prevention,
            Some(crate::bucket::PublicAccessPrevention::Inherited)
        );
        assert_eq!(
            update.uniform_bucket_level_access_enabled,
            Some(false.into())
        );
        let create = super::super::buckets::create_body(bucket, "").unwrap();
        for omitted in [
            "autoclass",
            "versioning",
            "retentionPolicy",
            "softDeletePolicy",
            "iamConfiguration",
        ] {
            assert!(create.get(omitted).is_none(), "{omitted}");
        }
    }

    #[test]
    fn grpc_masks_reject_unsupported_and_accept_defaults() {
        let status = Status::from(JsonError::new(
            hyper::StatusCode::NOT_IMPLEMENTED,
            "notImplemented",
            "Unsupported operation; use JSON instead",
        ));
        assert_eq!(status.code(), Code::Unimplemented);
        let details = <crate::grpc::generated::google::rpc::Status as prost::Message>::decode(
            status.details(),
        )
        .unwrap();
        assert_eq!(
            details.details[0].type_url,
            "type.googleapis.com/google.rpc.ErrorInfo"
        );
        assert_eq!(
            Status::from(JsonError::new(
                hyper::StatusCode::GONE,
                "gone",
                "Expired session; restart the request"
            ))
            .code(),
            Code::FailedPrecondition
        );
        // A member that versioning and iam_config do not have names no field (recorded
        // 2026-11-19, case grpc-probe-inferred-bucket-masks).
        for path in ["versioning.unknown", "iam_config.unknown"] {
            let error = bucket_patch(&pb::Bucket::default(), &[path.into()]).unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument, "{path}");
            assert_eq!(
                error.message(),
                format!("There is no field for this resource named '{path}'.")
            );
        }
        // The settings after `acl.entity` are those the JSON API does not store either.
        for path in [
            "*",
            "acl.entity",
            "rpo",
            "website",
            "logging",
            "encryption",
            "billing",
            "custom_placement_config",
            "hierarchical_namespace",
            "ip_filter",
            "satisfies_pzs",
        ] {
            assert_unsupported(
                bucket_patch(&pb::Bucket::default(), &[path.into()]),
                &format!("UpdateBucket update_mask {path}"),
            );
        }
        for path in [
            "acl.entity",
            "contexts",
            "contexts.custom",
            "retention.mode",
        ] {
            assert_unsupported(
                object_patch(&pb::Object::default(), &[path.into()]),
                &format!("UpdateObject update_mask {path}"),
            );
        }
        // Recorded 2026-11-18: UpdateObject takes `*` alone as a replacement and accepts an
        // output-only path without changing anything; `*` beside another path stays refused.
        assert_eq!(
            object_patch(&pb::Object::default(), &["*".into()])
                .unwrap()
                .write,
            crate::object::WriteMode::Replace
        );
        assert_eq!(
            object_patch(&pb::Object::default(), &["owner".into()]).unwrap(),
            Default::default()
        );
        assert_unsupported(
            object_patch(&pb::Object::default(), &["*".into(), "owner".into()]),
            "UpdateObject update_mask *",
        );
        assert_code(mask(None), Code::InvalidArgument);
        assert_code(
            mask(Some(&prost_types::FieldMask::default())),
            Code::InvalidArgument,
        );
        assert_code(
            strict_conditions(ObjectConditions {
                if_generation_match: Some(-1),
                ..Default::default()
            }),
            Code::InvalidArgument,
        );
        let b = pb::Bucket {
            labels: [
                ("keep".into(), "changed".into()),
                ("ignored".into(), "no".into()),
            ]
            .into(),
            ..Default::default()
        };
        assert_eq!(
            bucket_patch(&b, &["labels.keep".into()]).unwrap().labels,
            MapPatch::Merge([("keep".into(), Some("changed".into()))].into())
        );
        let update = object_patch(
            &pb::Object::default(),
            &["temporary_hold".into(), "event_based_hold".into()],
        )
        .unwrap();
        assert_eq!(update.temporary_hold, Some(false));
        assert_eq!(update.event_based_hold, Some(false));
    }

    #[tokio::test]
    async fn grpc_update_bucket_autoclass_masks() {
        let autoclass = |enabled, class: Option<&str>| pb::Bucket {
            autoclass: Some(pb::bucket::Autoclass {
                enabled,
                terminal_storage_class: class.map(str::to_owned),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            bucket_patch(&autoclass(true, Some("ARCHIVE")), &["autoclass".into()])
                .unwrap()
                .autoclass
                .unwrap(),
            crate::bucket::autoclass::AutoclassUpdate {
                enabled: Some(true),
                terminal_storage_class: Some(
                    crate::bucket::autoclass::TerminalStorageClass::Archive
                )
            }
        );
        assert_eq!(
            bucket_patch(
                &autoclass(true, Some("ARCHIVE")),
                &["autoclass.terminal_storage_class".into()]
            )
            .unwrap()
            .autoclass
            .unwrap(),
            crate::bucket::autoclass::AutoclassUpdate {
                enabled: None,
                terminal_storage_class: Some(
                    crate::bucket::autoclass::TerminalStorageClass::Archive
                )
            }
        );
        assert_eq!(
            bucket_patch(&pb::Bucket::default(), &["autoclass.enabled".into()])
                .unwrap()
                .autoclass
                .unwrap(),
            crate::bucket::autoclass::AutoclassUpdate {
                enabled: Some(false),
                terminal_storage_class: None
            }
        );
        let (s, _dir) = setup().await;
        let update = |bucket: pb::Bucket, paths: &[&str]| pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: "projects/_/buckets/update-copy-bucket".into(),
                ..bucket
            }),
            update_mask: fixtures::mask(paths),
            ..Default::default()
        };
        let enabled = s
            .update_bucket(
                Request::new(update(autoclass(true, None), &["autoclass"])),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let config = enabled.autoclass.unwrap();
        assert!(config.enabled);
        assert_eq!(config.terminal_storage_class.as_deref(), Some("NEARLINE"));
        assert!(
            config.toggle_time.is_some() && config.terminal_storage_class_update_time.is_some()
        );
        assert_eq!(enabled.storage_class, "STANDARD");
        let invalid = update(autoclass(true, Some("COLDLINE")), &["autoclass"]);
        assert_code(
            s.update_bucket(Request::new(invalid), None).await,
            tonic::Code::InvalidArgument,
        );
        let disabled = s
            .update_bucket(
                Request::new(update(autoclass(false, None), &["autoclass.enabled"])),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let config = disabled.autoclass.unwrap();
        assert!(!config.enabled);
        assert_eq!(config.terminal_storage_class, None);
        let error = s
            .update_bucket(
                Request::new(update(
                    autoclass(false, Some("ARCHIVE")),
                    &["autoclass.terminal_storage_class"],
                )),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("terminal_storage_class"));
        assert!(!error.message().contains("terminalStorageClass"));
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_lifecycle_size_rejection_precedes_bucket_updates_and_retention_lock() {
        let (storage, _dir) = setup().await;
        let bucket = "update-copy-bucket";
        let resource = "projects/_/buckets/update-copy-bucket";
        let before = storage
            .state
            .catalog
            .patch_bucket(
                bucket,
                BucketMetadataUpdate {
                    lifecycle: Some(crate::bucket::lifecycle::LifecycleRules::for_test(vec![
                        json!({"action":{"type":"Delete"},"condition":{"sizeAboveBytes":"1"}}),
                    ])),
                    retention_policy: RetentionPolicyUpdate::Set(
                        crate::bucket::RetentionPeriod::try_from(3600).unwrap(),
                    ),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        for metageneration in [None, Some(before.metageneration)] {
            let update = lifecycle_label_update(BucketConditions {
                if_metageneration_match: metageneration,
                ..Default::default()
            });
            assert_code(
                storage.update_bucket(Request::new(update), None).await,
                Code::Unimplemented,
            );
        }
        let lock = pb::LockBucketRetentionPolicyRequest {
            bucket: resource.into(),
            if_metageneration_match: before.metageneration,
        };
        assert_code(
            storage
                .lock_bucket_retention_policy(Request::new(lock), None)
                .await,
            Code::Unimplemented,
        );
        let after = storage
            .state
            .catalog
            .get_bucket(bucket, PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.metageneration, before.metageneration);
        assert_eq!(after.labels, before.labels);
        assert_eq!(after.lifecycle, before.lifecycle);
        assert!(!after.retention_policy.unwrap().is_locked);
        let clear = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["lifecycle"]),
            ..Default::default()
        };
        let cleared = storage
            .update_bucket(Request::new(clear), None)
            .await
            .unwrap()
            .into_inner();
        assert!(cleared.lifecycle.is_none());
        assert_eq!(cleared.metageneration, before.metageneration + 1);
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_lifecycle_precheck_rejects_concurrent_size_rules_without_its_own_mutation() {
        let (storage, _dir) = setup().await;
        let bucket = "update-copy-bucket";
        let before = storage
            .state
            .catalog
            .get_bucket(bucket, PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        let rule = json!({"action":{"type":"Delete"},"condition":{"sizeAboveBytes":"1"}});
        let writes = RefCell::new(VecDeque::from([
            json!({"lifecycle":{"rule":[rule.clone()]}}),
        ]));
        let request = lifecycle_label_update(BucketConditions::default());
        let result = BUCKET_PRECHECK_UPDATES
            .scope(writes, storage.update_bucket(Request::new(request), None))
            .await;
        assert_code(result, Code::Unimplemented);
        let after = storage
            .state
            .catalog
            .get_bucket(bucket, PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.metageneration, before.metageneration + 1);
        assert_eq!(after.labels, before.labels);
        assert_eq!(
            after.lifecycle,
            crate::bucket::lifecycle::LifecycleRules::for_test(vec![rule]).into()
        );
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_lifecycle_precheck_retries_only_implicit_matches() {
        for (matched, not_matched, failure) in [
            (None, None, None),
            (Some(1), None, Some(Code::FailedPrecondition)),
            (None, Some(2), Some(Code::FailedPrecondition)),
        ] {
            let (storage, _dir) = setup().await;
            let bucket = "update-copy-bucket";
            let before = storage
                .state
                .catalog
                .get_bucket(bucket, PERMISSIVE)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(before.metageneration, 1);
            let request = lifecycle_label_update(BucketConditions {
                if_metageneration_match: matched,
                if_metageneration_not_match: not_matched,
            });
            let writes = RefCell::new(VecDeque::from([json!({"defaultEventBasedHold":true})]));
            let result = BUCKET_PRECHECK_UPDATES
                .scope(writes, storage.update_bucket(Request::new(request), None))
                .await;
            let after = storage
                .state
                .catalog
                .get_bucket(bucket, PERMISSIVE)
                .await
                .unwrap()
                .unwrap();
            assert!(after.default_event_based_hold);
            if let Some(code) = failure {
                assert_code(result, code);
                assert_eq!(after.metageneration, 2);
                assert_eq!(after.labels, before.labels);
            } else {
                let response = result.unwrap().into_inner();
                assert_eq!(response.metageneration, 3);
                assert_eq!(after.metageneration, 3);
                assert_eq!(after.labels, [("changed".into(), "yes".into())].into());
            }
            storage.state.catalog.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn grpc_lifecycle_precheck_stops_after_three_conflicts() {
        let (storage, _dir) = setup().await;
        let bucket = "update-copy-bucket";
        let before = storage
            .state
            .catalog
            .get_bucket(bucket, PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        let writes = RefCell::new(VecDeque::from(vec![
            json!({"defaultEventBasedHold":true});
            4
        ]));
        let request = lifecycle_label_update(BucketConditions::default());
        let result = BUCKET_PRECHECK_UPDATES
            .scope(writes, storage.update_bucket(Request::new(request), None))
            .await;
        assert_code(result, Code::Aborted);
        let after = storage
            .state
            .catalog
            .get_bucket(bucket, PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.metageneration, before.metageneration + 3);
        assert_eq!(after.labels, before.labels);
        assert_eq!(after.lifecycle, before.lifecycle);
        assert!(after.default_event_based_hold);
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_retention_lock_and_compose_limits() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let lock = pb::LockBucketRetentionPolicyRequest {
            bucket: resource.into(),
            if_metageneration_match: 0,
        };
        assert_code(
            s.lock_bucket_retention_policy(Request::new(lock), None)
                .await,
            Code::InvalidArgument,
        );
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                retention_policy: Some(pb::bucket::RetentionPolicy {
                    retention_duration: Some(prost_types::Duration {
                        seconds: 3600,
                        nanos: 0,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["retention_policy"]),
            ..Default::default()
        };
        let b = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let lock = pb::LockBucketRetentionPolicyRequest {
            bucket: resource.into(),
            if_metageneration_match: b.metageneration,
        };
        assert!(
            s.lock_bucket_retention_policy(Request::new(lock), None)
                .await
                .unwrap()
                .into_inner()
                .retention_policy
                .unwrap()
                .is_locked
        );
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["retention_policy"]),
            ..Default::default()
        };
        assert_code(
            s.update_bucket(Request::new(request), None).await,
            Code::InvalidArgument,
        );
        let source = upload(&s, "tiny", 1).await;
        let src = pb::compose_object_request::SourceObject {
            name: "tiny".into(),
            generation: source.generation,
            ..Default::default()
        };
        let request = pb::ComposeObjectRequest {
            destination: Some(pb::Object {
                bucket: resource.into(),
                name: "thirty-two".into(),
                ..Default::default()
            }),
            source_objects: vec![src; 32],
            ..Default::default()
        };
        let composed = s
            .compose_object(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(composed.size, 32);
        assert_eq!(composed.component_count, 32);
        s.state.catalog.close().await.unwrap();
    }

    /// The proto rounds any `nanos` of a retention duration down to the nearest second
    /// (storage.proto lines 2429-2433), so a period under one second is no period at all.
    #[tokio::test]
    async fn grpc_bucket_update_rounds_retention_nanos_down() {
        let (s, _dir) = setup().await;
        let retention = |seconds, nanos| pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: RESOURCE.into(),
                retention_policy: Some(pb::bucket::RetentionPolicy {
                    retention_duration: Some(prost_types::Duration { seconds, nanos }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["retention_policy.retention_duration"]),
            ..Default::default()
        };
        let updated = s
            .update_bucket(Request::new(retention(3600, 999_999_999)), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            updated.retention_policy.unwrap().retention_duration,
            Some(prost_types::Duration {
                seconds: 3600,
                nanos: 0
            })
        );
        let stored = stored_bucket(&s).await;
        assert_eq!(stored.retention_policy.unwrap().retention_period, 3600);
        let status = assert_code(
            s.update_bucket(Request::new(retention(0, 500_000_000)), None)
                .await,
            Code::InvalidArgument,
        );
        assert!(status.message().contains("retention"), "{status}");
        assert_eq!(
            stored_bucket(&s).await.metageneration,
            updated.metageneration
        );
        // Durations outside the canonical form, whose nanos have the other sign than the seconds
        // or reach a whole second, round down by their value: the stored period, or `None` for
        // INVALID_ARGUMENT.
        for (seconds, nanos, period) in [
            (3600, -1, Some(3599)),
            (1, -1, None),
            (0, 1_500_000_000, Some(1)),
        ] {
            let before = stored_bucket(&s).await;
            let result = s
                .update_bucket(Request::new(retention(seconds, nanos)), None)
                .await;
            let stored = stored_bucket(&s).await;
            let Some(period) = period else {
                assert_code(result, Code::InvalidArgument);
                assert_eq!(stored, before, "{seconds} {nanos}");
                continue;
            };
            assert_eq!(
                result
                    .unwrap()
                    .into_inner()
                    .retention_policy
                    .unwrap()
                    .retention_duration,
                Some(prost_types::Duration {
                    seconds: period,
                    nanos: 0
                }),
                "{seconds} {nanos}"
            );
            assert_eq!(
                stored.retention_policy.unwrap().retention_period,
                u64::try_from(period).unwrap()
            );
        }
        s.state.catalog.close().await.unwrap();
    }

    /// Cloud Storage ignores the ETag an UpdateBucket request carries in its bucket: an update
    /// naming a superseded ETag applied (recorded 2026-11-18), so only the request's
    /// metageneration conditions make an update conditional.
    #[tokio::test]
    async fn grpc_bucket_update_ignores_the_bucket_etag() {
        let (s, _dir) = setup().await;
        let first = stored_bucket(&s).await;
        let first_etag = convert::bucket(&first, convert::Lifecycle::Omit)
            .unwrap()
            .etag;
        let update = |etag: &str| {
            let mut request = lifecycle_label_update(BucketConditions::default());
            request.bucket.as_mut().unwrap().etag = etag.into();
            request.update_mask = fixtures::mask(&["labels"]);
            request
        };
        let mut metageneration = first.metageneration;
        // The current ETag, the one it superseded, and a value that is no ETag.
        for etag in [first_etag.as_str(), first_etag.as_str(), "not-an-etag"] {
            let updated = s
                .update_bucket(Request::new(update(etag)), None)
                .await
                .unwrap()
                .into_inner();
            metageneration += 1;
            assert_eq!(updated.metageneration, metageneration, "{etag}");
        }
        s.state.catalog.close().await.unwrap();
    }

    /// Strict LockBucketRetentionPolicy needs storage.buckets.update on the bucket, checked
    /// before the bucket is looked up; a refused lock leaves the policy unlocked.
    #[tokio::test]
    async fn grpc_retention_lock_requires_bucket_update_permission() {
        let dir = crate::test_support::TestDir::new();
        let catalog = crate::storage::Catalog::open(&dir.0).await.unwrap();
        let definition = json::parse_insert(
            br#"{"name":"locking","retentionPolicy":{"retentionPeriod":"3600"}}"#,
            "p",
            std::time::SystemTime::now(),
        )
        .unwrap();
        catalog.insert_bucket(definition).await.unwrap();
        let s = Storage {
            compat_profile: crate::config::CompatProfile::Native,
            state: std::sync::Arc::new(crate::server::State::for_test(
                catalog,
                crate::auth::Auth::strict(fixtures::registry(&[
                    ("admin", "p", "roles/storage.admin"),
                    ("viewer", "p", "roles/storage.objectViewer"),
                    ("reader", "", ""),
                ])),
            )),
            max_object_bytes: 1024,
        };
        let lock = |bucket: &str| pb::LockBucketRetentionPolicyRequest {
            bucket: format!("projects/_/buckets/{bucket}"),
            if_metageneration_match: 1,
        };
        for bucket in ["locking", "missing-bucket"] {
            let status = assert_code(
                s.lock_bucket_retention_policy(Request::new(lock(bucket)), Some(Caller::Anonymous))
                    .await,
                Code::Unauthenticated,
            );
            assert_eq!(status.message(), "Login Required");
            for token in ["reader", "viewer"] {
                let status = assert_code(
                    s.lock_bucket_retention_policy(
                        Request::new(lock(bucket)),
                        fixtures::caller(&s, token).await,
                    )
                    .await,
                    Code::PermissionDenied,
                );
                assert_eq!(
                    status.message(),
                    format!(
                        "{token}@example.invalid does not have storage.buckets.update access to the Google Cloud Storage bucket. Permission 'storage.buckets.update' denied on resource (or it may not exist)."
                    )
                );
            }
        }
        let stored = s
            .state
            .catalog
            .get_bucket("locking", PERMISSIVE)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.metageneration, 1);
        assert!(!stored.retention_policy.unwrap().is_locked);
        let admin = fixtures::caller(&s, "admin").await;
        assert_code(
            s.lock_bucket_retention_policy(Request::new(lock("missing-bucket")), admin)
                .await,
            Code::NotFound,
        );
        let locked = s
            .lock_bucket_retention_policy(Request::new(lock("locking")), admin)
            .await
            .unwrap()
            .into_inner();
        assert!(locked.retention_policy.unwrap().is_locked);
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_updates_reject_predefined_acls_without_changing_metadata() {
        let (s, _dir) = setup().await;
        let before = stored_bucket(&s).await;
        for (acl, default_acl) in [("publicRead", ""), ("", "bucketOwnerRead")] {
            let request = pb::UpdateBucketRequest {
                predefined_acl: acl.into(),
                predefined_default_object_acl: default_acl.into(),
                ..lifecycle_label_update(BucketConditions::default())
            };
            assert_unsupported(
                s.update_bucket(Request::new(request), None).await,
                "predefined bucket ACLs",
            );
        }
        let after = stored_bucket(&s).await;
        assert_eq!(after.metageneration, before.metageneration);
        assert_eq!(after.labels, before.labels);
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_retention_lock_requires_a_policy_and_the_current_metageneration() {
        let (s, _dir) = setup().await;
        let lock = |metageneration| pb::LockBucketRetentionPolicyRequest {
            bucket: RESOURCE.into(),
            if_metageneration_match: metageneration,
        };
        let unlocked = stored_bucket(&s).await;
        // A bucket without a policy has nothing to lock, whatever the metageneration says.
        let status = assert_code(
            s.lock_bucket_retention_policy(Request::new(lock(unlocked.metageneration)), None)
                .await,
            Code::InvalidArgument,
        );
        assert!(status.message().contains("retention"), "{status}");
        assert_eq!(stored_bucket(&s).await, unlocked);

        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: RESOURCE.into(),
                retention_policy: Some(pb::bucket::RetentionPolicy {
                    retention_duration: Some(prost_types::Duration {
                        seconds: 3600,
                        nanos: 0,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["retention_policy"]),
            ..Default::default()
        };
        let configured = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_code(
            s.lock_bucket_retention_policy(Request::new(lock(configured.metageneration - 1)), None)
                .await,
            Code::FailedPrecondition,
        );
        assert!(
            stored_bucket(&s)
                .await
                .retention_policy
                .is_some_and(|policy| !policy.is_locked)
        );

        let locked = s
            .lock_bucket_retention_policy(Request::new(lock(configured.metageneration)), None)
            .await
            .unwrap()
            .into_inner();
        assert!(locked.retention_policy.as_ref().unwrap().is_locked);
        assert_eq!(locked.metageneration, configured.metageneration + 1);
        // A lock cannot be taken twice, even at the current metageneration.
        let status = assert_code(
            s.lock_bucket_retention_policy(Request::new(lock(locked.metageneration)), None)
                .await,
            Code::InvalidArgument,
        );
        assert!(status.message().contains("already locked"), "{status}");
        assert_eq!(
            stored_bucket(&s).await.metageneration,
            locked.metageneration
        );
        s.state.catalog.close().await.unwrap();
    }
}
