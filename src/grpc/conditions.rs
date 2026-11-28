//! Precondition fields of the Storage request messages, read by name so the conditions cannot
//! trade places.

use super::{generated::google::storage::v2, v1::pb as v1};
use crate::{bucket::BucketConditions, object::ObjectConditions};

macro_rules! object_conditions {
    ($($request:ty),+ $(,)?) => {$(
        impl From<&$request> for ObjectConditions {
            fn from(request: &$request) -> Self {
                Self {
                    if_generation_match: request.if_generation_match,
                    if_generation_not_match: request.if_generation_not_match,
                    if_metageneration_match: request.if_metageneration_match,
                    if_metageneration_not_match: request.if_metageneration_not_match,
                }
            }
        }
    )+};
}

object_conditions!(
    v2::GetObjectRequest,
    v2::ReadObjectRequest,
    v2::DeleteObjectRequest,
    v2::BidiReadObjectSpec,
    v2::WriteObjectSpec,
    v2::UpdateObjectRequest,
    v2::RewriteObjectRequest,
    v2::MoveObjectRequest,
    v2::RestoreObjectRequest,
    v1::GetObjectRequest,
    v1::DeleteObjectRequest,
    v1::UpdateObjectRequest,
    v1::PatchObjectRequest,
    v1::InsertObjectSpec,
);

/// A request that names a source object, whose `if_source_*` fields hold the conditions on it.
/// It is a trait because the request messages belong to the generated crate.
pub(crate) trait SourceConditions {
    /// The conditions on the source object.
    fn source_conditions(&self) -> ObjectConditions;
}

macro_rules! source_conditions {
    ($($request:ty),+ $(,)?) => {$(
        impl SourceConditions for $request {
            fn source_conditions(&self) -> ObjectConditions {
                ObjectConditions {
                    if_generation_match: self.if_source_generation_match,
                    if_generation_not_match: self.if_source_generation_not_match,
                    if_metageneration_match: self.if_source_metageneration_match,
                    if_metageneration_not_match: self.if_source_metageneration_not_match,
                }
            }
        }
    )+};
}

source_conditions!(v2::RewriteObjectRequest, v2::MoveObjectRequest);

macro_rules! bucket_conditions {
    ($($request:ty),+ $(,)?) => {$(
        impl From<&$request> for BucketConditions {
            fn from(request: &$request) -> Self {
                Self {
                    if_metageneration_match: request.if_metageneration_match,
                    if_metageneration_not_match: request.if_metageneration_not_match,
                }
            }
        }
    )+};
}

bucket_conditions!(
    v2::GetBucketRequest,
    v2::DeleteBucketRequest,
    v2::UpdateBucketRequest,
    v1::GetBucketRequest,
    v1::DeleteBucketRequest,
    v1::UpdateBucketRequest,
);

/// A compose destination takes only the two match conditions.
impl From<&v2::ComposeObjectRequest> for ObjectConditions {
    fn from(request: &v2::ComposeObjectRequest) -> Self {
        Self {
            if_generation_match: request.if_generation_match,
            if_metageneration_match: request.if_metageneration_match,
            ..Self::default()
        }
    }
}

/// A compose destination takes only the two match conditions.
impl From<&v1::ComposeObjectRequest> for ObjectConditions {
    fn from(request: &v1::ComposeObjectRequest) -> Self {
        Self {
            if_generation_match: request.if_generation_match,
            if_metageneration_match: request.if_metageneration_match,
            ..Self::default()
        }
    }
}

/// A compose source takes only a generation match.
impl From<&v2::compose_object_request::source_object::ObjectPreconditions> for ObjectConditions {
    fn from(
        preconditions: &v2::compose_object_request::source_object::ObjectPreconditions,
    ) -> Self {
        Self {
            if_generation_match: preconditions.if_generation_match,
            ..Self::default()
        }
    }
}

/// A compose source takes only a generation match.
impl From<&v1::compose_object_request::source_objects::ObjectPreconditions> for ObjectConditions {
    fn from(
        preconditions: &v1::compose_object_request::source_objects::ObjectPreconditions,
    ) -> Self {
        Self {
            if_generation_match: preconditions.if_generation_match,
            ..Self::default()
        }
    }
}

/// An append names its generation, so only the metageneration conditions apply.
impl From<&v2::AppendObjectSpec> for ObjectConditions {
    fn from(spec: &v2::AppendObjectSpec) -> Self {
        Self {
            if_metageneration_match: spec.if_metageneration_match,
            if_metageneration_not_match: spec.if_metageneration_not_match,
            ..Self::default()
        }
    }
}

/// Locking requires the metageneration the caller read, so the field is not optional.
impl From<&v2::LockBucketRetentionPolicyRequest> for BucketConditions {
    fn from(request: &v2::LockBucketRetentionPolicyRequest) -> Self {
        Self {
            if_metageneration_match: Some(request.if_metageneration_match),
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISTINCT: ObjectConditions = ObjectConditions {
        if_generation_match: Some(1),
        if_generation_not_match: Some(2),
        if_metageneration_match: Some(3),
        if_metageneration_not_match: Some(4),
    };

    #[test]
    fn object_requests_keep_each_condition_in_its_field() {
        let request = v2::GetObjectRequest {
            if_generation_match: Some(1),
            if_generation_not_match: Some(2),
            if_metageneration_match: Some(3),
            if_metageneration_not_match: Some(4),
            ..Default::default()
        };
        assert_eq!(ObjectConditions::from(&request), DISTINCT);
        let request = v2::RewriteObjectRequest {
            if_source_generation_match: Some(1),
            if_source_generation_not_match: Some(2),
            if_source_metageneration_match: Some(3),
            if_source_metageneration_not_match: Some(4),
            if_generation_match: Some(5),
            ..Default::default()
        };
        assert_eq!(request.source_conditions(), DISTINCT);
        assert_eq!(
            ObjectConditions::from(&request),
            ObjectConditions {
                if_generation_match: Some(5),
                ..Default::default()
            }
        );
    }

    #[test]
    fn bucket_requests_keep_each_condition_in_its_field() {
        let request = v2::GetBucketRequest {
            if_metageneration_match: Some(3),
            if_metageneration_not_match: Some(4),
            ..Default::default()
        };
        assert_eq!(
            BucketConditions::from(&request),
            BucketConditions {
                if_metageneration_match: Some(3),
                if_metageneration_not_match: Some(4),
            }
        );
        let request = v2::LockBucketRetentionPolicyRequest {
            if_metageneration_match: 3,
            ..Default::default()
        };
        assert_eq!(
            BucketConditions::from(&request),
            BucketConditions {
                if_metageneration_match: Some(3),
                if_metageneration_not_match: None,
            }
        );
    }

    #[test]
    fn partial_condition_sets_take_only_their_fields() {
        let only_matches = ObjectConditions {
            if_generation_match: Some(1),
            if_metageneration_match: Some(3),
            ..Default::default()
        };
        let request = v2::ComposeObjectRequest {
            if_generation_match: Some(1),
            if_metageneration_match: Some(3),
            ..Default::default()
        };
        assert_eq!(ObjectConditions::from(&request), only_matches);
        let request = v1::ComposeObjectRequest {
            if_generation_match: Some(1),
            if_metageneration_match: Some(3),
            ..Default::default()
        };
        assert_eq!(ObjectConditions::from(&request), only_matches);

        let generation_match = ObjectConditions {
            if_generation_match: Some(1),
            ..Default::default()
        };
        let preconditions = v2::compose_object_request::source_object::ObjectPreconditions {
            if_generation_match: Some(1),
        };
        assert_eq!(ObjectConditions::from(&preconditions), generation_match);
        let preconditions = v1::compose_object_request::source_objects::ObjectPreconditions {
            if_generation_match: Some(1),
        };
        assert_eq!(ObjectConditions::from(&preconditions), generation_match);

        let spec = v2::AppendObjectSpec {
            if_metageneration_match: Some(3),
            if_metageneration_not_match: Some(4),
            ..Default::default()
        };
        assert_eq!(
            ObjectConditions::from(&spec),
            ObjectConditions {
                if_metageneration_match: Some(3),
                if_metageneration_not_match: Some(4),
                ..Default::default()
            }
        );
    }
}
