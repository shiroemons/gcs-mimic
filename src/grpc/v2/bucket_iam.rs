//! Bucket IAM policy RPCs: GetIamPolicy, SetIamPolicy and TestIamPermissions.

use super::{Storage, iam, names, status::unsupported};
use crate::{
    auth::{self, Caller},
    bucket::BucketAccess,
    json,
    server::authorize_bucket,
};
use tonic::{Request, Response, Status};

/// The bucket of an IAM resource. Managed folders and objects are valid IAM resources
/// without a local policy store, so they are unsupported rather than malformed.
fn iam_bucket(resource: &str, profile: crate::config::CompatProfile) -> Result<&str, Status> {
    if let Some((bucket, child)) = resource
        .strip_prefix("projects/_/buckets/")
        .and_then(|rest| rest.split_once('/'))
        && (child.starts_with("managedFolders/") || child.starts_with("objects/"))
    {
        crate::bucket::name::validate_for_profile(bucket, profile)?;
        return Err(unsupported(
            "managed folder and object IAM policies; use a bucket resource such as projects/_/buckets/example-bucket",
        ));
    }
    let bucket = names::bucket_name(resource)?;
    crate::bucket::name::validate_for_profile(bucket, profile)?;
    Ok(bucket)
}

fn policy(policy: auth::Policy) -> Result<iam::Policy, Status> {
    Ok(iam::Policy {
        version: 1,
        bindings: policy.bindings.into_iter().map(Into::into).collect(),
        etag: policy
            .etag
            .map(|v| {
                json::decode_hash::<8>(&v).map(|v| v.to_vec()).map_err(|_| {
                    Status::internal("Invalid stored policy etag; restore a valid catalog backup")
                })
            })
            .transpose()?
            .unwrap_or_default(),
        ..Default::default()
    })
}

impl Storage {
    pub(super) async fn get_iam_policy(
        &self,
        request: Request<iam::GetIamPolicyRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::Policy>, Status> {
        let r = request.into_inner();
        let bucket = iam_bucket(&r.resource, self.compat_profile)?;
        // Policies here carry no conditions, so every version renders the same bindings. Cloud
        // Storage served version 2 as well (case grpc-unsupported-options).
        if r.options
            .is_some_and(|o| !matches!(o.requested_policy_version, 0..=3))
        {
            return Err(Status::invalid_argument(
                "requested_policy_version must be between 0 and 3",
            ));
        }
        let access = self.access(caller, bucket, auth::BUCKETS_GET_IAM).await?;
        Ok(Response::new(policy(
            self.state.catalog.get_policy(bucket, &access).await?,
        )?))
    }
    pub(super) async fn set_iam_policy(
        &self,
        request: Request<iam::SetIamPolicyRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::Policy>, Status> {
        let r = request.into_inner();
        let bucket = iam_bucket(&r.resource, self.compat_profile)?;
        if let Some(mask) = r.update_mask
            && !mask.paths.is_empty()
            && !(mask.paths.len() == 2
                && mask.paths.iter().any(|p| p == "bindings")
                && mask.paths.iter().any(|p| p == "etag"))
        {
            return Err(unsupported(
                "SetIamPolicy update_mask; omit the mask to replace bindings and etag",
            ));
        }
        let access = self.access(caller, bucket, auth::BUCKETS_SET_IAM).await?;
        let p = r.policy.ok_or_else(|| {
            Status::invalid_argument(
                "policy is required; supply bindings, for example an empty list",
            )
        })?;
        if !p.audit_configs.is_empty() || p.bindings.iter().any(|b| b.condition.is_some()) {
            return Err(unsupported(
                "IAM conditions or audit configs; supply unconditional bindings",
            ));
        }
        let etag = if !p.etag.is_empty() {
            let bytes: [u8; 8] = p.etag.try_into().map_err(|_| {
                Status::failed_precondition(
                    "Policy etag does not match; read the current policy before updating",
                )
            })?;
            Some(crate::etag::policy_etag(i64::from_be_bytes(bytes)))
        } else {
            None
        };
        let p = auth::policy_input::PolicyInput {
            version: p.version,
            bindings: p.bindings.into_iter().map(Into::into).collect(),
            etag,
        }
        .into_policy()?;
        Ok(Response::new(policy(
            self.state.catalog.set_policy(bucket, p, &access).await?,
        )?))
    }
    pub(super) async fn test_iam_permissions(
        &self,
        request: Request<iam::TestIamPermissionsRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<iam::TestIamPermissionsResponse>, Status> {
        let r = request.into_inner();
        let bucket = iam_bucket(&r.resource, self.compat_profile)?;
        if r.permissions.is_empty() {
            return Err(Status::invalid_argument(
                "permissions is required; supply at least one permission",
            ));
        }
        let mut requested = Vec::new();
        for permission in r.permissions {
            let permission = json::check_testable_permission(&permission)?;
            if !requested.contains(&permission) {
                requested.push(permission);
            }
        }
        self.state
            .catalog
            .get_policy(bucket, &BucketAccess::Permissive)
            .await?;
        let mut permissions = Vec::new();
        for p in requested {
            match authorize_bucket(&self.state, caller, p, bucket).await {
                Ok(access)
                    if self
                        .state
                        .catalog
                        .has_bucket_permission(bucket, &access)
                        .await? =>
                {
                    permissions.push(p.as_str().to_owned())
                }
                Ok(_) => {}
                Err(e) if e.status == hyper::StatusCode::FORBIDDEN => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Response::new(iam::TestIamPermissionsResponse {
            permissions,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::bucket_update::from_proto as bucket_patch;
    use super::super::pb;
    use super::super::test_support::{RESOURCE, setup};
    use super::*;
    use crate::bucket::BucketMetadataUpdate;
    use crate::grpc::test_support::{self as fixtures, assert_code, assert_unsupported};
    use crate::storage::test_fixtures::PERMISSIVE;
    use tonic::Code;

    #[tokio::test]
    async fn grpc_iam_validates_typed_policy_and_permission_values() {
        let (storage, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        for permission in [
            "",
            auth::BUCKETS_CREATE.as_str(),
            "storage.invalid",
            "storage.objects.get&permissions=storage.objects.delete",
        ] {
            let error = storage
                .test_iam_permissions(
                    Request::new(iam::TestIamPermissionsRequest {
                        resource: resource.into(),
                        permissions: vec![permission.into()],
                    }),
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(error.code(), Code::InvalidArgument);
        }
        let response = storage
            .test_iam_permissions(
                Request::new(iam::TestIamPermissionsRequest {
                    resource: resource.into(),
                    permissions: vec![auth::OBJECTS_GET.as_str().into(); 1024],
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.permissions, [auth::OBJECTS_GET.as_str()]);
        assert_code(
            storage
                .test_iam_permissions(
                    Request::new(iam::TestIamPermissionsRequest {
                        resource: resource.into(),
                        permissions: vec!["storage.anywhereCaches.create".into()],
                    }),
                    None,
                )
                .await,
            Code::Unimplemented,
        );
        let response = storage
            .set_iam_policy(
                Request::new(iam::SetIamPolicyRequest {
                    resource: resource.into(),
                    policy: Some(iam::Policy {
                        version: 3,
                        bindings: vec![iam::Binding {
                            role: "roles/storage.objectViewer".into(),
                            members: vec!["allUsers".into()],
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.bindings[0].members, ["allUsers"]);
        assert_eq!(response.etag.len(), 8);
        storage.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_iam_config_parent_update_resets_pap_and_keeps_absent_uniform_access() {
        let empty = bucket_patch(&pb::Bucket::default(), &["iam_config".into()]).unwrap();
        assert_eq!(empty.uniform_bucket_level_access_enabled, None);
        assert_eq!(
            empty.public_access_prevention,
            Some(crate::bucket::PublicAccessPrevention::Inherited)
        );
        assert!(empty.touched);
        assert_eq!(
            bucket_patch(
                &pb::Bucket::default(),
                &["iam_config.uniform_bucket_level_access.enabled".into()]
            )
            .unwrap()
            .uniform_bucket_level_access_enabled,
            Some(false.into())
        );
        let (s, _dir) = setup().await;
        let resource = "projects/_/buckets/update-copy-bucket";
        s.state
            .catalog
            .patch_bucket(
                "update-copy-bucket",
                BucketMetadataUpdate {
                    public_access_prevention: Some(crate::bucket::PublicAccessPrevention::Enforced),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let request = pb::UpdateBucketRequest {
            bucket: Some(pb::Bucket {
                name: resource.into(),
                iam_config: Some(pb::bucket::IamConfig {
                    uniform_bucket_level_access: Some(
                        pb::bucket::iam_config::UniformBucketLevelAccess {
                            enabled: true,
                            ..Default::default()
                        },
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            update_mask: fixtures::mask(&["iam_config"]),
            ..Default::default()
        };
        let b = s
            .update_bucket(Request::new(request), None)
            .await
            .unwrap()
            .into_inner();
        let i = b.iam_config.unwrap();
        assert_eq!(i.public_access_prevention, "inherited");
        assert!(i.uniform_bucket_level_access.unwrap().enabled);
        let update = |mask: &'static str, iam_config: pb::bucket::IamConfig| {
            s.update_bucket(
                Request::new(pb::UpdateBucketRequest {
                    bucket: Some(pb::Bucket {
                        name: resource.into(),
                        iam_config: Some(iam_config),
                        ..Default::default()
                    }),
                    update_mask: fixtures::mask(&[mask]),
                    ..Default::default()
                }),
                None,
            )
        };
        // A leaf path leaves the other setting alone.
        let b = update(
            "iam_config.public_access_prevention",
            pb::bucket::IamConfig {
                public_access_prevention: "enforced".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_inner();
        let i = b.iam_config.unwrap();
        assert_eq!(i.public_access_prevention, "enforced");
        assert!(i.uniform_bucket_level_access.unwrap().enabled);
        // Under the parent path, public access prevention alone keeps uniform access, as Cloud
        // Storage did (recorded 2026-11-13).
        let b = update(
            "iam_config",
            pb::bucket::IamConfig {
                public_access_prevention: "enforced".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_inner();
        let i = b.iam_config.unwrap();
        assert_eq!(i.public_access_prevention, "enforced");
        assert!(i.uniform_bucket_level_access.unwrap().enabled);
        // An empty message resets public access prevention, keeps uniform access and still
        // advances the metageneration (recorded).
        let before = b.metageneration;
        let b = update("iam_config", pb::bucket::IamConfig::default())
            .await
            .unwrap()
            .into_inner();
        assert_eq!(b.metageneration, before + 1);
        let i = b.iam_config.unwrap();
        assert_eq!(i.public_access_prevention, "inherited");
        assert!(i.uniform_bucket_level_access.unwrap().enabled);
        let after = s
            .get_bucket(
                Request::new(pb::GetBucketRequest {
                    name: resource.into(),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert_eq!(after.metageneration, b.metageneration);
        assert_eq!(after.iam_config, Some(i));
        s.state.catalog.close().await.unwrap();
    }

    /// Bucket policies here carry no conditions and no audit configs, on any protocol, so a
    /// policy that sets either is refused with ErrorInfo and the stored policy stays as it was.
    #[tokio::test]
    async fn grpc_iam_set_refuses_conditional_bindings_and_audit_configs() {
        let (s, _dir) = setup().await;
        let get = || iam::GetIamPolicyRequest {
            resource: RESOURCE.into(),
            options: None,
        };
        let before = s
            .get_iam_policy(Request::new(get()), None)
            .await
            .unwrap()
            .into_inner();
        let binding = || iam::Binding {
            role: "roles/storage.objectViewer".into(),
            members: vec!["allUsers".into()],
            condition: None,
        };
        let conditional = iam::Policy {
            version: 3,
            bindings: vec![
                binding(),
                iam::Binding {
                    condition: Some(crate::grpc::generated::google::r#type::Expr {
                        expression: "request.time < timestamp(\"2030-01-01T00:00:00Z\")".into(),
                        title: "expires".into(),
                        ..Default::default()
                    }),
                    ..binding()
                },
            ],
            ..Default::default()
        };
        let audited = iam::Policy {
            bindings: vec![binding()],
            audit_configs: vec![iam::AuditConfig {
                service: "storage.googleapis.com".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        for policy in [conditional, audited] {
            assert_unsupported(
                s.set_iam_policy(
                    Request::new(iam::SetIamPolicyRequest {
                        resource: RESOURCE.into(),
                        policy: Some(policy),
                        update_mask: None,
                    }),
                    None,
                )
                .await,
                "IAM conditions or audit configs",
            );
        }
        let after = s
            .get_iam_policy(Request::new(get()), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(after, before);
        s.state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_iam_rejects_partial_masks_and_child_resources_without_changing_the_policy() {
        let (s, _dir) = setup().await;
        let get = |resource: &str, version| iam::GetIamPolicyRequest {
            resource: resource.into(),
            options: Some(iam::GetPolicyOptions {
                requested_policy_version: version,
            }),
        };
        let before = s
            .get_iam_policy(Request::new(get(RESOURCE, 3)), None)
            .await
            .unwrap()
            .into_inner();
        // Without conditions every version renders the same policy, as Cloud Storage's did.
        for version in [0, 1, 2] {
            let policy = s
                .get_iam_policy(Request::new(get(RESOURCE, version)), None)
                .await
                .unwrap()
                .into_inner();
            assert_eq!(policy, before, "version {version}");
        }
        assert_code(
            s.get_iam_policy(Request::new(get(RESOURCE, 4)), None).await,
            Code::InvalidArgument,
        );
        let replacement = iam::Policy {
            bindings: vec![iam::Binding {
                role: "roles/storage.objectViewer".into(),
                members: vec!["allUsers".into()],
                condition: None,
            }],
            ..Default::default()
        };
        let set = |resource: &str, paths: &[&str]| iam::SetIamPolicyRequest {
            resource: resource.into(),
            policy: Some(replacement.clone()),
            update_mask: fixtures::mask(paths),
        };
        for paths in [&["bindings"][..], &["etag"], &["bindings", "version"]] {
            assert_unsupported(
                s.set_iam_policy(Request::new(set(RESOURCE, paths)), None)
                    .await,
                "SetIamPolicy update_mask",
            );
        }
        for child in [
            "projects/_/buckets/update-copy-bucket/managedFolders/folder",
            "projects/_/buckets/update-copy-bucket/objects/object",
        ] {
            assert_unsupported(
                s.get_iam_policy(Request::new(get(child, 3)), None).await,
                "managed folder and object IAM policies",
            );
            assert_unsupported(
                s.set_iam_policy(Request::new(set(child, &[])), None).await,
                "managed folder and object IAM policies",
            );
            assert_unsupported(
                s.test_iam_permissions(
                    Request::new(iam::TestIamPermissionsRequest {
                        resource: child.into(),
                        permissions: vec![auth::OBJECTS_GET.as_str().into()],
                    }),
                    None,
                )
                .await,
                "managed folder and object IAM policies",
            );
        }
        let unchanged = s
            .get_iam_policy(Request::new(get(RESOURCE, 3)), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(unchanged, before);
        let replaced = s
            .set_iam_policy(Request::new(set(RESOURCE, &["bindings", "etag"])), None)
            .await
            .unwrap()
            .into_inner();
        assert_eq!(replaced.bindings, replacement.bindings);
        assert_ne!(replaced.etag, before.etag);
        s.state.catalog.close().await.unwrap();
    }
}
