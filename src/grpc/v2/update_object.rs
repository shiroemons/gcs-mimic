//! Object metadata updates: UpdateObject.

use super::{
    Storage, access_status, mask::mask, object_update::canned_acl, pb, status::unsupported,
};
use crate::{
    auth::{self, Caller},
    grpc::validate::{self, NegativeGeneration},
    object::{ObjectConditions, ObjectError, generation::Generation},
};
use tonic::{Request, Response, Status};

#[cfg(test)]
mod event_hold_release;
#[cfg(test)]
mod object_etag;
#[cfg(test)]
mod update_masks;

impl Storage {
    pub(super) async fn update_object(
        &self,
        request: Request<pb::UpdateObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::Object>, Status> {
        let mut r = request.into_inner();
        let key = super::customer_key::key(r.common_object_request_params.as_ref())?;
        // Cloud Storage applied predefined_acl on UpdateObject (case grpc-unsupported-options,
        // recorded 2026-11-13).
        let predefined_acl = canned_acl("predefined_acl", &r.predefined_acl)?;
        let retention_override = if r.override_unlocked_retention {
            crate::object::retention::UnlockedOverride::Requested
        } else {
            crate::object::retention::UnlockedOverride::NotRequested
        };
        let o = r.object.take().ok_or_else(|| {
            Status::invalid_argument("object is required; supply object.bucket and object.name")
        })?;
        let object = self.object_key(&o.bucket, &o.name)?;
        let (bucket, name) = object.parts();
        let generation =
            validate::selected_generation(o.generation).map_err(|NegativeGeneration| {
                Status::invalid_argument("generation must be positive or omitted")
            })?;
        // `Object.etag` sets no condition, though the proto says an update "is only performed if
        // the etag matches that of the live object": Cloud Storage applied an update that
        // carried a superseded etag (recorded 2026-11-18).
        let paths = mask(r.update_mask.as_ref())?;
        let mut update = super::object_update::from_proto(&o, paths)?;
        update.acl = std::mem::take(&mut update.acl)
            .with_predefined(predefined_acl)
            .map_err(|_| unsupported("UpdateObject acl together with predefined_acl; omit one"))?;
        let access = self.access(caller, bucket, auth::OBJECTS_UPDATE).await?;
        // Cloud Storage compares whatever conditions arrive: a metageneration match of 0 and a
        // match beside a not-match fail as conditions that do not hold (recorded 2026-11-18).
        let c = ObjectConditions::from(&r);
        crate::server::authorize_retention_update(
            &self.state.catalog,
            &access,
            bucket,
            &mut update,
            retention_override,
        )
        .await
        .map_err(|e| access_status(e, caller))?;
        let record = self
            .state
            .catalog
            .update_object_metadata(
                object.into_target(generation.map(Generation::get)),
                c,
                update,
                // A wrong key fails before the metadata change commits.
                move |record| {
                    crate::csek::check_metadata(record.customer_key_sha256, key)
                        .map(drop)
                        .map_err(ObjectError::CustomerEncryption)
                },
                &access,
            )
            .await
            .map_err(super::object_io::naming_object(bucket, name))?;
        // A metadata update never changes the stored key, so the key the closure verified
        // against the pre-commit record also holds for the committed one.
        let hashes = crate::csek::StoredHashes::for_key(record.customer_key_sha256, key);
        Ok(Response::new(
            self.written_object(&record, hashes, caller).await?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::convert;
    use super::super::test_support::{
        RESOURCE, grants, setup, stored_object, undispatched, upload, upload_to,
    };
    use super::*;
    use crate::bucket::{BucketRecord, RetentionPolicy};
    use crate::grpc::test_support::{
        self as fixtures, assert_code, assert_unsupported, caller, registry,
    };
    use crate::storage::test_fixtures::{PERMISSIVE, live};
    use crate::{auth::Auth, storage::Catalog, test_support::TestDir};
    use std::sync::Arc;
    use tonic::Code;

    #[tokio::test]
    async fn grpc_update_object_retention() {
        let (s, _dir) = setup().await;
        let mut record =
            BucketRecord::for_test("retention-objects", "local-project", s.state.clock.now());
        record.object_retention_enabled = true;
        s.state.catalog.insert_bucket(record).await.unwrap();
        upload_to(&s, "retention-objects", "kept", 3).await;
        upload(&s, "plain", 3).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let request =
            |bucket: &str, name: &str, seconds: Option<i64>, force: bool| pb::UpdateObjectRequest {
                object: Some(pb::Object {
                    bucket: format!("projects/_/buckets/{bucket}"),
                    name: name.into(),
                    retention: seconds.map(|seconds| pb::object::Retention {
                        mode: pb::object::retention::Mode::Unlocked as i32,
                        retain_until_time: Some(prost_types::Timestamp {
                            seconds: now + seconds,
                            nanos: 0,
                        }),
                    }),
                    ..Default::default()
                }),
                update_mask: fixtures::mask(&["retention"]),
                override_unlocked_retention: force,
                ..Default::default()
            };
        assert_code(
            s.update_object(
                Request::new(request("update-copy-bucket", "plain", Some(3600), false)),
                None,
            )
            .await,
            Code::InvalidArgument,
        );
        let object = s
            .update_object(
                Request::new(request("retention-objects", "kept", Some(3600), false)),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let retention = object.retention.unwrap();
        assert_eq!(retention.mode, pb::object::retention::Mode::Unlocked as i32);
        for invalid in [
            pb::object::retention::Mode::Unspecified as i32,
            pb::object::retention::Mode::Unlocked as i32,
        ] {
            let mut bad = request("retention-objects", "kept", Some(7200), true);
            let retention = bad.object.as_mut().unwrap().retention.as_mut().unwrap();
            retention.mode = invalid;
            if invalid != pb::object::retention::Mode::Unspecified as i32 {
                retention.retain_until_time = None;
            }
            assert_code(
                s.update_object(Request::new(bad), None).await,
                Code::InvalidArgument,
            );
        }
        assert_eq!(retention.retain_until_time.unwrap().seconds, now + 3600);
        // Deleting a retained object is 403 in JSON, so PERMISSION_DENIED with the same text
        // (observed 2026-10-31).
        let denied = s
            .delete_object(
                Request::new(pb::DeleteObjectRequest {
                    bucket: "projects/_/buckets/retention-objects".into(),
                    object: "kept".into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.code(), Code::PermissionDenied);
        assert!(
            denied.message().starts_with(
                "Object 'retention-objects/kept' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until "
            ),
            "{}",
            denied.message()
        );
        // Shortening or removing an Unlocked configuration requires the override and is 403 in
        // JSON, so PERMISSION_DENIED here with the same text (observed 2026-10-31).
        for seconds in [Some(60), None] {
            let denied = s
                .update_object(
                    Request::new(request("retention-objects", "kept", seconds, false)),
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(denied.code(), Code::PermissionDenied, "{seconds:?}");
            assert_eq!(
                denied.message(),
                crate::object::UNLOCKED_RETENTION_OVERRIDE_MESSAGE
            );
        }
        let current = s
            .state
            .catalog
            .get_object(
                live("retention-objects", "kept"),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(
            convert::object(
                &current,
                crate::csek::StoredHashes::keyless(current.customer_key_sha256)
            )
            .unwrap()
            .retention
            .unwrap()
            .retain_until_time
            .unwrap()
            .seconds,
            now + 3600
        );
        let shortened = s
            .update_object(
                Request::new(request("retention-objects", "kept", Some(60), true)),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            shortened
                .retention
                .unwrap()
                .retain_until_time
                .unwrap()
                .seconds,
            now + 60
        );
        let removed = s
            .update_object(
                Request::new(request("retention-objects", "kept", None, true)),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(removed.retention.is_none());
        s.state.catalog.close().await.unwrap();
    }

    /// Cloud Storage returned `retention.retain_until_time` with its nanoseconds over gRPC and
    /// `retention_expire_time` at millisecond precision (observed 2026-10-31).
    #[tokio::test]
    async fn grpc_retention_keeps_nanoseconds_and_expire_time_is_milliseconds() {
        // Not a multiple of 100 ns, which a platform flooring a SystemTime would lose.
        const FINE_NANOS: i32 = 123_456_789;
        let (s, _dir) = setup().await;
        let mut record =
            BucketRecord::for_test("retention-ns", "local-project", s.state.clock.now());
        record.object_retention_enabled = true;
        s.state.catalog.insert_bucket(record).await.unwrap();
        upload_to(&s, "retention-ns", "kept", 3).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let object = s
            .update_object(
                Request::new(pb::UpdateObjectRequest {
                    object: Some(pb::Object {
                        bucket: "projects/_/buckets/retention-ns".into(),
                        name: "kept".into(),
                        retention: Some(pb::object::Retention {
                            mode: pb::object::retention::Mode::Unlocked as i32,
                            retain_until_time: Some(prost_types::Timestamp {
                                seconds: now + 3600,
                                nanos: FINE_NANOS,
                            }),
                        }),
                        ..Default::default()
                    }),
                    update_mask: fixtures::mask(&["retention"]),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        let until = object.retention.unwrap().retain_until_time.unwrap();
        assert_eq!((until.seconds, until.nanos), (now + 3600, FINE_NANOS));
        let expire = object.retention_expire_time.unwrap();
        assert_eq!((expire.seconds, expire.nanos), (now + 3600, 123_000_000));
        s.state.catalog.close().await.unwrap();
    }

    /// With a bucket retention policy, `retention_expire_time` is also cut to the millisecond
    /// (observed 2026-10-31) while the denial text keeps the microsecond creation time.
    #[tokio::test]
    async fn grpc_bucket_policy_expire_time_is_milliseconds() {
        let (s, _dir) = setup().await;
        let now = s.state.clock.now();
        let record = BucketRecord {
            retention_policy: Some(RetentionPolicy {
                retention_period: 3600,
                effective_time: now,
                is_locked: false,
            }),
            ..BucketRecord::for_test("policy-ms", "local-project", now)
        };
        s.state.catalog.insert_bucket(record).await.unwrap();
        let object = upload_to(&s, "policy-ms", "kept", 3).await;
        let expire = convert::object(
            &object,
            crate::csek::StoredHashes::keyless(object.customer_key_sha256),
        )
        .unwrap()
        .retention_expire_time
        .unwrap();
        assert_eq!(expire.nanos % 1_000_000, 0);
        let created = object
            .time_created
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!(expire.seconds, created.as_secs() as i64 + 3600);
        assert_eq!(expire.nanos as u32, created.subsec_millis() * 1_000_000);
        let denied = s
            .delete_object(
                Request::new(pb::DeleteObjectRequest {
                    bucket: "projects/_/buckets/policy-ms".into(),
                    object: "kept".into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(denied.code(), Code::PermissionDenied);
        assert_eq!(
            denied.message(),
            format!(
                "Object 'policy-ms/kept' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until {}",
                crate::object::retention::pacific_rfc3339(
                    crate::object::unix_nanos::UnixNanos::from(
                        object.time_created + std::time::Duration::from_secs(3600)
                    )
                )
            )
        );
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_update_object_retention_strict() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let mut record =
            BucketRecord::for_test("update-copy-bucket", "p", std::time::SystemTime::now());
        record.object_retention_enabled = true;
        catalog.insert_bucket(record).await.unwrap();
        let s = Storage {
            compat_profile: crate::profile::CompatProfile::Native,
            state: Arc::new(crate::server::State::for_test(
                catalog,
                Auth::strict(registry(&[
                    ("admin", "p", "roles/storage.objectAdmin"),
                    ("user", "p", "roles/storage.objectUser"),
                ])),
            )),
            max_object_bytes: 1024,
        };
        upload(&s, "object", 3).await;
        let until = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        let request = pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: "projects/_/buckets/update-copy-bucket".into(),
                name: "object".into(),
                retention: Some(pb::object::Retention {
                    mode: pb::object::retention::Mode::Unlocked as i32,
                    retain_until_time: Some(prost_types::Timestamp {
                        seconds: until,
                        nanos: 0,
                    }),
                }),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["retention"]),
            ..Default::default()
        };
        // objectUser may update objects but not set retention.
        assert_code(
            s.update_object(Request::new(request.clone()), caller(&s, "user").await)
                .await,
            Code::PermissionDenied,
        );
        assert!(
            s.state
                .catalog
                .get_object(
                    live("update-copy-bucket", "object"),
                    Default::default(),
                    PERMISSIVE
                )
                .await
                .unwrap()
                .retention
                .is_none()
        );
        s.update_object(Request::new(request), caller(&s, "admin").await)
            .await
            .unwrap();
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_update_object_acl() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let source = upload(&s, "acl-object", 3).await;
        let acl_request = |entity: &str| pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: resource.into(),
                name: "acl-object".into(),
                acl: vec![pb::ObjectAccessControl {
                    entity: entity.into(),
                    role: "READER".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["acl"]),
            ..Default::default()
        };
        let object = s
            .update_object(Request::new(acl_request("allUsers")), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(object.metageneration, source.metageneration.get() + 1);
        assert!(
            object
                .acl
                .iter()
                .any(|e| e.entity == "allUsers" && e.role == "READER" && !e.etag.is_empty())
        );
        let stored = s
            .state
            .catalog
            .object_acl_projection(
                "update-copy-bucket",
                vec![("acl-object".into(), object.generation)],
                PERMISSIVE,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(
            stored.objects[0]
                .entries()
                .iter()
                .any(|e| e.entity.as_str() == "allUsers")
        );
        assert_code(
            s.update_object(Request::new(acl_request("nobody")), None)
                .await,
            Code::InvalidArgument,
        );
        // Uniform bucket-level access disables object ACLs, exactly as in JSON.
        s.update_bucket(
            Request::new(pb::UpdateBucketRequest {
                bucket: Some(pb::Bucket {
                    name: resource.into(),
                    iam_config: Some(pb::bucket::IamConfig {
                        uniform_bucket_level_access: Some(
                            pb::bucket::iam_config::UniformBucketLevelAccess {
                                enabled: true,
                                lock_time: None,
                            },
                        ),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                update_mask: fixtures::mask(&["iam_config"]),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
        assert_code(
            s.update_object(Request::new(acl_request("allAuthenticatedUsers")), None)
                .await,
            Code::InvalidArgument,
        );
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_map_replacement_does_not_add_preconditions() {
        let (storage, _dir) = setup().await;
        upload(&storage, "object", 10).await;
        for index in 0..20 {
            let replace = pb::UpdateObjectRequest {
                object: Some(pb::Object {
                    bucket: "projects/_/buckets/update-copy-bucket".into(),
                    name: "object".into(),
                    metadata: [("value".into(), index.to_string())].into(),
                    ..Default::default()
                }),
                update_mask: fixtures::mask(&["metadata"]),
                ..Default::default()
            };
            let patch = pb::UpdateObjectRequest {
                object: Some(pb::Object {
                    bucket: "projects/_/buckets/update-copy-bucket".into(),
                    name: "object".into(),
                    content_type: "application/octet-stream".into(),
                    ..Default::default()
                }),
                update_mask: fixtures::mask(&["content_type"]),
                ..Default::default()
            };
            let (replaced, patched) = tokio::join!(
                storage.update_object(Request::new(replace), None),
                storage.update_object(Request::new(patch), None),
            );
            assert_eq!(replaced.unwrap().into_inner().metadata.len(), 1);
            patched.unwrap();
            let replace = pb::UpdateBucketRequest {
                bucket: Some(pb::Bucket {
                    name: "projects/_/buckets/update-copy-bucket".into(),
                    labels: [("value".into(), index.to_string())].into(),
                    ..Default::default()
                }),
                update_mask: fixtures::mask(&["labels"]),
                ..Default::default()
            };
            let patch = pb::UpdateBucketRequest {
                bucket: Some(pb::Bucket {
                    name: "projects/_/buckets/update-copy-bucket".into(),
                    default_event_based_hold: false,
                    ..Default::default()
                }),
                update_mask: fixtures::mask(&["default_event_based_hold"]),
                ..Default::default()
            };
            let (replaced, patched) = tokio::join!(
                storage.update_bucket(Request::new(replace), None),
                storage.update_bucket(Request::new(patch), None),
            );
            assert_eq!(replaced.unwrap().into_inner().labels.len(), 1);
            patched.unwrap();
        }
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_update_masks() {
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                labels: [("new".into(), "yes".into())].into(),
                versioning: Some(pb::bucket::Versioning { enabled: true }),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["labels"]),
            ..Default::default()
        };
        let b = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(b.labels.len(), 1);
        assert_eq!(b.labels["new"], "yes");
        assert!(b.versioning.is_none());
        let stale = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                ..Default::default()
            }),
            if_metageneration_match: Some(1),
            update_mask: fixtures::mask(&["labels"]),
            ..Default::default()
        };
        assert_code(
            s.update_bucket(Request::new(stale), None).await,
            Code::FailedPrecondition,
        );
        let source = upload(&s, "object", 10).await;
        let request = pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: resource.into(),
                name: "object".into(),
                metadata: [("keep".into(), "yes".into())].into(),
                content_type: "ignored".into(),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["metadata"]),
            if_generation_match: Some(source.generation.get()),
            ..Default::default()
        };
        let object = s
            .update_object(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(object.content_type, "text/plain");
        assert_eq!(object.metadata["keep"], "yes");
        // A `metadata` mask merges into the stored map, unlike the bucket `labels` mask above.
        let request = pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: resource.into(),
                name: "object".into(),
                metadata: [("added".into(), "yes".into())].into(),
                temporary_hold: true,
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["metadata", "temporary_hold"]),
            ..Default::default()
        };
        let object = s
            .update_object(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let merged = std::collections::HashMap::from([
            ("keep".into(), "yes".into()),
            ("added".into(), "yes".into()),
        ]);
        assert_eq!(object.metadata, merged);
        assert!(object.temporary_hold);
        let read = s
            .state
            .catalog
            .get_object(
                live("update-copy-bucket", "object"),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(read.metadata, merged.into_iter().collect());
        assert_eq!(read.temporary_hold.as_bool(), Some(true));
        s.state.catalog.close().await.unwrap();
    }

    /// UpdateObject and RewriteObject apply a canned ACL as their JSON counterparts do, as Cloud
    /// Storage applied `private` on both (case grpc-unsupported-options, recorded 2026-11-13).
    #[tokio::test]
    async fn grpc_object_updates_and_rewrites_apply_predefined_acls() {
        let (s, _dir) = setup().await;
        let source = upload(&s, "source", 4).await;
        let update = |predefined_acl: &str, paths: &[&str], acl| pb::UpdateObjectRequest {
            object: Some(pb::Object {
                bucket: RESOURCE.into(),
                name: "source".into(),
                content_type: "application/json".into(),
                acl,
                ..Default::default()
            }),
            predefined_acl: predefined_acl.into(),
            update_mask: fixtures::mask(paths),
            ..Default::default()
        };
        let updated = s
            .update_object(
                Request::new(update("publicRead", &["content_type"], Vec::new())),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(updated.metageneration, source.metageneration.get() + 1);
        assert!(
            grants(&updated).contains(&("allUsers", "READER")),
            "{updated:?}"
        );
        let explicit = vec![pb::ObjectAccessControl {
            entity: "allAuthenticatedUsers".into(),
            role: "READER".into(),
            ..Default::default()
        }];
        assert_unsupported(
            s.update_object(Request::new(update("private", &["acl"], explicit)), None)
                .await,
            "UpdateObject acl together with predefined_acl",
        );
        let status = s
            .update_object(
                Request::new(update("publicReadWrite", &["content_type"], Vec::new())),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            stored_object(&s, "source")
                .await
                .unwrap()
                .metageneration
                .get(),
            updated.metageneration
        );

        let rewrite = pb::RewriteObjectRequest {
            source_bucket: RESOURCE.into(),
            source_object: "source".into(),
            destination_bucket: RESOURCE.into(),
            destination_name: "rewritten".into(),
            destination_predefined_acl: "private".into(),
            ..Default::default()
        };
        let rewritten = s
            .rewrite_object(undispatched(rewrite), None)
            .await
            .unwrap()
            .into_inner()
            .resource
            .unwrap();
        // `private` leaves only the owner's grant.
        let owner = rewritten.owner.as_ref().unwrap().entity.as_str();
        assert_eq!(grants(&rewritten), [(owner, "OWNER")], "{rewritten:?}");
        s.state.catalog.close().await.unwrap();
    }
}
