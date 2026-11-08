//! The typed record of a strict-mode decision that refused a Cloud Storage permission.
//!
//! Adapters render it in Cloud Storage's denial form and log it at debug level when the
//! response leaves, so a user can tell which identity lacked which permission.

use super::{Identity, Permission};
use crate::diagnostics::{self, LogLevel};
use serde::Serialize;
use std::fmt;

/// Who a refused request acted as.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Principal {
    Anonymous,
    /// A registered identity's id: the token holder, the trusted-listener principal or the
    /// signer of a signed request.
    Registered(Box<str>),
}

/// The resource a refused permission applies to. The name is only logged: the message names
/// the resource kind alone, so a refusal reveals nothing about whether the resource exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Resource {
    /// A project-level decision; `None` when no single project was evaluated.
    Project(Option<Box<str>>),
    Bucket(Box<str>),
    Object {
        bucket: Box<str>,
    },
}

impl Resource {
    /// The resource a permission evaluated in `bucket` applies to: the object for object
    /// operations, the bucket for listing and bucket operations, and a project for HMAC key
    /// operations. An HMAC key belongs to a project, never to a bucket, and the bucket does not
    /// tell which project, so that project stays unnamed.
    pub(crate) fn in_bucket(permission: Permission, bucket: &str) -> Self {
        match permission {
            Permission::ObjectsCreate
            | Permission::ObjectsDelete
            | Permission::ObjectsGet
            | Permission::ObjectsUpdate
            | Permission::ObjectsRestore
            | Permission::ObjectsSetRetention
            | Permission::ObjectsOverrideUnlockedRetention
            | Permission::ObjectsGetIam
            | Permission::ObjectsSetIam
            | Permission::MultipartCreate
            | Permission::MultipartAbort
            | Permission::MultipartListParts => Self::Object {
                bucket: bucket.into(),
            },
            Permission::BucketsCreate
            | Permission::BucketsDelete
            | Permission::BucketsGet
            | Permission::BucketsUpdate
            | Permission::BucketsList
            | Permission::BucketsRestore
            | Permission::BucketsGetIam
            | Permission::BucketsSetIam
            | Permission::BucketsEnableObjectRetention
            | Permission::ObjectsList
            | Permission::MultipartList => Self::Bucket(bucket.into()),
            Permission::HmacKeysCreate
            | Permission::HmacKeysDelete
            | Permission::HmacKeysGet
            | Permission::HmacKeysList
            | Permission::HmacKeysUpdate => Self::Project(None),
        }
    }

    pub(crate) fn project(project: Option<&str>) -> Self {
        Self::Project(project.map(Into::into))
    }

    /// The resource as Cloud Storage's denial message names it.
    fn noun(&self) -> &'static str {
        match self {
            Self::Project(_) => "Google Cloud project",
            Self::Bucket(_) => "Google Cloud Storage bucket",
            Self::Object { .. } => "Google Cloud Storage object",
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Project(_) => "project",
            Self::Bucket(_) => "bucket",
            Self::Object { .. } => "object",
        }
    }
}

/// A permission that strict authorization refused, with the principal it refused and the
/// resource the permission applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denied {
    principal: Principal,
    permission: Permission,
    resource: Resource,
}

/// The `authorization_denied` event. Credentials never reach a denial, so the event cannot
/// carry tokens, signatures, keys or object contents.
#[derive(Serialize)]
struct DenialEvent<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    principal: Option<&'a str>,
    anonymous: bool,
    permission: &'static str,
    resource: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    bucket: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a str>,
}

impl Denied {
    /// `identity` is `None` for an anonymous caller. Boxed so that the error enums carrying a
    /// denial stay small.
    pub(crate) fn new(
        identity: Option<&Identity>,
        permission: Permission,
        resource: Resource,
    ) -> Box<Self> {
        let principal = identity.map_or(Principal::Anonymous, |identity| {
            Principal::Registered(identity.id().into())
        });
        Box::new(Self {
            principal,
            permission,
            resource,
        })
    }

    pub(crate) fn is_anonymous(&self) -> bool {
        self.principal == Principal::Anonymous
    }

    /// The same refusal attributed to an anonymous caller, keeping the permission and resource.
    pub(crate) fn into_anonymous(mut self: Box<Self>) -> Box<Self> {
        self.principal = Principal::Anonymous;
        self
    }

    #[cfg(test)]
    pub(crate) fn permission(&self) -> Permission {
        self.permission
    }

    /// The `authorization_denied` event's fields, as `log` writes them.
    #[cfg(test)]
    pub(crate) fn event_value(&self) -> serde_json::Value {
        serde_json::to_value(self.event()).unwrap()
    }

    /// Writes the `authorization_denied` debug event. The level check comes first, so a run
    /// at info or above pays one atomic load per refused response.
    pub(crate) fn log(&self) {
        if diagnostics::enabled(LogLevel::Debug) {
            diagnostics::emit(LogLevel::Debug, "authorization_denied", self.event());
        }
    }

    fn event(&self) -> DenialEvent<'_> {
        let (bucket, project) = match &self.resource {
            Resource::Project(project) => (None, project.as_deref()),
            Resource::Bucket(bucket) | Resource::Object { bucket } => (Some(&**bucket), None),
        };
        DenialEvent {
            principal: match &self.principal {
                Principal::Anonymous => None,
                Principal::Registered(id) => Some(id),
            },
            anonymous: self.is_anonymous(),
            permission: self.permission.as_str(),
            resource: self.resource.kind(),
            bucket,
            project,
        }
    }
}

/// Cloud Storage's denial form. Only the project-level sentence and an object-level first
/// sentence are documented; the bucket noun and the suffix on bucket and object denials are
/// local choices (see SPEC-017).
impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.principal {
            Principal::Anonymous => f.write_str("Anonymous caller")?,
            Principal::Registered(id) => f.write_str(id)?,
        }
        write!(
            f,
            " does not have {permission} access to the {noun}. Permission '{permission}' denied on resource (or it may not exist).",
            permission = self.permission,
            noun = self.resource.noun(),
        )
    }
}

impl std::error::Error for Denied {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{BUCKETS_CREATE, BUCKETS_GET, OBJECTS_CREATE, OBJECTS_GET, OBJECTS_LIST};

    fn identity(id: &str) -> Identity {
        Identity::from_upload_snapshot(&serde_json::json!({"id": id, "projects": {}})).unwrap()
    }

    #[test]
    fn messages_name_the_principal_the_permission_and_the_resource_kind() {
        let viewer = identity("viewer@example.invalid");
        for (identity, permission, resource, expected) in [
            (
                Some(&viewer),
                OBJECTS_GET,
                Resource::in_bucket(OBJECTS_GET, "bucket"),
                "viewer@example.invalid does not have storage.objects.get access to the Google Cloud Storage object. Permission 'storage.objects.get' denied on resource (or it may not exist).",
            ),
            (
                Some(&viewer),
                BUCKETS_GET,
                Resource::in_bucket(BUCKETS_GET, "bucket"),
                "viewer@example.invalid does not have storage.buckets.get access to the Google Cloud Storage bucket. Permission 'storage.buckets.get' denied on resource (or it may not exist).",
            ),
            (
                Some(&viewer),
                BUCKETS_CREATE,
                Resource::project(Some("project-a")),
                "viewer@example.invalid does not have storage.buckets.create access to the Google Cloud project. Permission 'storage.buckets.create' denied on resource (or it may not exist).",
            ),
            (
                None,
                OBJECTS_CREATE,
                Resource::in_bucket(OBJECTS_CREATE, "bucket"),
                "Anonymous caller does not have storage.objects.create access to the Google Cloud Storage object. Permission 'storage.objects.create' denied on resource (or it may not exist).",
            ),
            (
                None,
                OBJECTS_LIST,
                Resource::in_bucket(OBJECTS_LIST, "bucket"),
                "Anonymous caller does not have storage.objects.list access to the Google Cloud Storage bucket. Permission 'storage.objects.list' denied on resource (or it may not exist).",
            ),
        ] {
            let denied = Denied::new(identity, permission, resource);
            assert_eq!(denied.to_string(), expected);
            assert_eq!(denied.is_anonymous(), identity.is_none());
            assert_eq!(denied.permission(), permission);
        }
    }

    /// The bucket name stays out of the message: a refusal for a missing bucket must read
    /// exactly like one for an existing bucket.
    #[test]
    fn the_message_does_not_depend_on_the_bucket_name() {
        let viewer = identity("viewer@example.invalid");
        let message = |bucket| {
            Denied::new(
                Some(&viewer),
                OBJECTS_GET,
                Resource::in_bucket(OBJECTS_GET, bucket),
            )
            .to_string()
        };
        assert_eq!(message("bucket-a"), message("missing-bucket"));
        assert!(!message("bucket-a").contains("bucket-a"));
    }

    #[test]
    fn every_permission_applies_to_a_bucket_an_object_in_it_or_a_project() {
        for permission in Permission::ALL {
            let resource = Resource::in_bucket(*permission, "b");
            let object = permission.as_str().starts_with("storage.objects.")
                && *permission != OBJECTS_LIST
                || matches!(
                    permission,
                    Permission::MultipartCreate
                        | Permission::MultipartAbort
                        | Permission::MultipartListParts
                );
            let expected = if permission.as_str().starts_with("storage.hmacKeys.") {
                Resource::Project(None)
            } else if object {
                Resource::Object { bucket: "b".into() }
            } else {
                Resource::Bucket("b".into())
            };
            assert_eq!(resource, expected, "{permission}");
        }
    }

    /// An HMAC key belongs to a project, so its refusal names the project in the message and
    /// in the event, never the bucket the check happened to run in.
    #[test]
    fn hmac_key_refusals_name_the_project() {
        let viewer = identity("viewer@example.invalid");
        for permission in [
            Permission::HmacKeysCreate,
            Permission::HmacKeysDelete,
            Permission::HmacKeysGet,
            Permission::HmacKeysList,
            Permission::HmacKeysUpdate,
        ] {
            let denied = Denied::new(
                Some(&viewer),
                permission,
                Resource::in_bucket(permission, "bucket-a"),
            );
            assert_eq!(
                denied.to_string(),
                format!(
                    "viewer@example.invalid does not have {permission} access to the Google Cloud project. Permission '{permission}' denied on resource (or it may not exist)."
                )
            );
            assert_eq!(
                denied.event_value(),
                serde_json::json!({
                    "principal": "viewer@example.invalid",
                    "anonymous": false,
                    "permission": permission.as_str(),
                    "resource": "project",
                })
            );
        }
    }

    #[test]
    fn the_event_names_the_principal_permission_resource_and_location_only() {
        let viewer = identity("viewer@example.invalid");
        let event = |denied: Box<Denied>| denied.event_value();
        assert_eq!(
            event(Denied::new(
                Some(&viewer),
                OBJECTS_GET,
                Resource::in_bucket(OBJECTS_GET, "bucket-a"),
            )),
            serde_json::json!({
                "principal": "viewer@example.invalid",
                "anonymous": false,
                "permission": "storage.objects.get",
                "resource": "object",
                "bucket": "bucket-a",
            })
        );
        assert_eq!(
            event(Denied::new(
                None,
                BUCKETS_GET,
                Resource::in_bucket(BUCKETS_GET, "bucket-a")
            )),
            serde_json::json!({
                "anonymous": true,
                "permission": "storage.buckets.get",
                "resource": "bucket",
                "bucket": "bucket-a",
            })
        );
        assert_eq!(
            event(Denied::new(
                Some(&viewer),
                BUCKETS_CREATE,
                Resource::project(Some("project-a")),
            )),
            serde_json::json!({
                "principal": "viewer@example.invalid",
                "anonymous": false,
                "permission": "storage.buckets.create",
                "resource": "project",
                "project": "project-a",
            })
        );
        assert_eq!(
            event(Denied::new(None, BUCKETS_GET, Resource::project(None))),
            serde_json::json!({
                "anonymous": true,
                "permission": "storage.buckets.get",
                "resource": "project",
            })
        );
    }
}
